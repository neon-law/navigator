//! Durable scheduling and provider publication for Blog Posts.
//!
//! Each destination runs as journaled steps. Instagram and Threads publish
//! through a media container: the container is created once, its readiness is
//! polled with durable sleeps, and only a ready container is published. Every
//! step that writes to a provider re-checks the deployment's egress gate and
//! re-resolves the Brand's account immediately before its request, and the one
//! public write is claimed in the store first, so a replayed step never sends
//! again.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::config::SocialEgressGate;
use store::social_accounts::SocialProvider;
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows::social_publish::{send_credential, SendCredential, TokenRefreshConfig};
use workflows::{SocialPublishReport, SocialPublishRequest};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Destination {
    pub(crate) provider: SocialProvider,
    pub(crate) account_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PostSnapshot {
    pub(crate) brand_key: String,
    title: String,
    description: String,
    video_asset: Option<String>,
    #[serde(default)]
    vertical_video: bool,
    #[serde(default)]
    tiktok_ai_generated: Option<bool>,
    publish_at: Option<DateTime<Utc>>,
    status: String,
    facebook_caption: Option<String>,
    instagram_caption: Option<String>,
    threads_caption: Option<String>,
    youtube_caption: Option<String>,
    pub(crate) destinations: Vec<Destination>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PublishOutcome {
    pub(crate) provider: SocialProvider,
    pub(crate) state: String,
    /// The provider's receipt for a confirmed post whose public link has not
    /// yet been read back.
    #[serde(default)]
    pub(crate) link_pending: Option<String>,
    /// A known failure the provider asked to be retried later: a rate limit
    /// or a temporary error. Nothing was published.
    #[serde(default)]
    pub(crate) retryable: bool,
}

const CONFIRMED: &str = "confirmed";
pub(crate) const FAILED: &str = "failed";
pub(crate) const UNKNOWN: &str = "unknown";
/// A destination this deployment may not write to. It is recorded as a known
/// pre-send failure and does not hold back the website article.
const BLOCKED: &str = "blocked";

impl PublishOutcome {
    pub(crate) fn new(provider: SocialProvider, state: &str) -> Self {
        Self {
            provider,
            state: state.into(),
            link_pending: None,
            retryable: false,
        }
    }
}

/// The journaled result of one destination step: continue with a value, or
/// stop with the destination's outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Step<T> {
    Continue(T),
    Settled(PublishOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ContainerStatus {
    Ready,
    Processing,
}

/// How long a provider may take to prepare a media container.
///
/// Instagram documents polling a container's `status_code` once a minute for
/// no more than five minutes; Threads recommends waiting about 30 seconds
/// before publishing and reports readiness through `status`.
#[derive(Clone, Copy)]
pub(crate) struct ContainerPolicy {
    pub(crate) interval: Duration,
    pub(crate) checks: usize,
}

pub(crate) const fn container_policy(provider: SocialProvider) -> Option<ContainerPolicy> {
    match provider {
        SocialProvider::Instagram => Some(ContainerPolicy {
            interval: Duration::from_secs(60),
            checks: 5,
        }),
        SocialProvider::Threads => Some(ContainerPolicy {
            interval: Duration::from_secs(30),
            checks: 10,
        }),
        SocialProvider::Facebook
        | SocialProvider::Youtube
        | SocialProvider::Linkedin
        | SocialProvider::X
        | SocialProvider::Tiktok => None,
    }
}

#[derive(Clone)]
pub struct SocialPublisherService {
    publisher: Publisher,
}

impl SocialPublisherService {
    /// The publisher both engines drive. The DBOS twin registers it so the
    /// provider steps and store claims have exactly one implementation.
    #[must_use]
    pub fn publisher(&self) -> Publisher {
        self.publisher.clone()
    }
}

#[derive(Clone)]
pub struct SocialPublishReconcilerService {
    surreal: SurrealDb,
    publisher: Publisher,
}

impl SocialPublishReconcilerService {
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        kms: Arc<dyn cloud::RuntimeKms>,
        storage: Arc<dyn cloud::StorageService>,
    ) -> Self {
        let publisher = SocialPublisherService::new(surreal.clone(), kms, storage).publisher;
        Self { surreal, publisher }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileRequest {
    pub tick: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub started: usize,
}

#[restate_sdk::workflow(name = "SocialPublishReconciler")]
impl SocialPublishReconcilerService {
    #[restate_sdk::handler]
    #[allow(clippy::too_many_lines)]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(request): Json<ReconcileRequest>,
    ) -> Result<Json<ReconcileReport>, HandlerError> {
        let surreal = self.surreal.clone();
        let scheduled = ctx
            .run(move || async move {
                store::social_publications::due_or_future_scheduled(&surreal, 100)
                    .await
                    .map(Json)
                    .map_err(HandlerError::from)
            })
            .name("load-scheduled-social-posts")
            .await?
            .into_inner();
        let Some(ingress) = std::env::var("RESTATE_BROKER_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            return Err(TerminalError::new("Restate ingress is not configured").into());
        };
        let auth = std::env::var("RESTATE_AUTH_TOKEN").ok();
        for (post_id, publish_at) in &scheduled {
            let request = SocialPublishRequest {
                post_id: *post_id,
                publish_at: *publish_at,
            };
            let key = format!("{post_id}:{}", publish_at.to_rfc3339());
            let ingress = ingress.clone();
            let auth = auth.clone();
            ctx.run(move || async move {
                workflows::start_workflow(
                    &ingress,
                    auth.as_deref(),
                    "SocialPublisher",
                    &key,
                    "run",
                    &request,
                    true,
                )
                .await
                .map(|_| ())
                .map_err(|error| HandlerError::from(error.to_string()))
            })
            .name("start-scheduled-social-publication")
            .await?;
        }
        let surreal = self.surreal.clone();
        let alerts = ctx
            .run(move || async move {
                store::inbox_alerts::awaiting_reconciliation(&surreal, 15)
                    .await
                    .map(Json)
                    .map_err(HandlerError::from)
            })
            .name("load-pending-inbound-notices")
            .await?
            .into_inner();
        for message_id in &alerts {
            let message_id = *message_id;
            let key = format!("{message_id}:{}", request.tick);
            let ingress = ingress.clone();
            let auth = auth.clone();
            ctx.run(move || async move {
                workflows::start_workflow(
                    &ingress,
                    auth.as_deref(),
                    "InboundMessageAlert",
                    &key,
                    "run",
                    &workflows::InboundMessageAlertRequest { message_id },
                    true,
                )
                .await
                .map(|_| ())
                .map_err(|error| HandlerError::from(error.to_string()))
            })
            .name("recover-inbound-message-notice")
            .await?;
        }
        let publisher = self.publisher.clone();
        ctx.run(move || async move { publisher.reconcile_youtube_links().await.map(Json) })
            .name("reconcile-youtube-public-links")
            .await?;
        Ok(Json(ReconcileReport {
            started: scheduled.len() + alerts.len(),
        }))
    }
}

impl SocialPublisherService {
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        kms: Arc<dyn cloud::RuntimeKms>,
        storage: Arc<dyn cloud::StorageService>,
    ) -> Self {
        Self {
            publisher: Publisher {
                surreal,
                kms,
                storage,
                egress: SocialEgressGate::from_process_env(),
                refresh: TokenRefreshConfig::from_env(),
                endpoints: ProviderEndpoints::default(),
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(60))
                    .build()
                    .unwrap_or_default(),
                video_url: views::assets::bucket_asset_url,
            },
        }
    }
}

/// The destination set and copy of a still-scheduled Post, read back from the
/// store at run time. `None` when the Post is gone, no longer scheduled, or
/// rescheduled since the request was made. Shared by the Restate handler and
/// the DBOS twin's `load-scheduled-post` step.
pub(crate) async fn scheduled_post_snapshot(
    surreal: &SurrealDb,
    post_id: Uuid,
    requested_at: DateTime<Utc>,
) -> Result<Option<PostSnapshot>, SnapshotError> {
    let post = store::posts::find_by_id(surreal, post_id).await?;
    let Some(post) = post else {
        return Ok(None);
    };
    if post.status != "scheduled" || post.publish_at != Some(requested_at) {
        return Ok(None);
    }
    let accounts = store::social_accounts::selected_for_brand(
        surreal,
        &post.brand_key,
        &post.selected_social_accounts,
    )
    .await?;
    Ok(Some(PostSnapshot {
        brand_key: post.brand_key,
        title: post.title,
        description: post.description,
        video_asset: post.video_asset,
        vertical_video: post.vertical_video,
        tiktok_ai_generated: post.tiktok_ai_generated,
        publish_at: post.publish_at,
        status: post.status,
        facebook_caption: post.facebook_caption,
        instagram_caption: post.instagram_caption,
        threads_caption: post.threads_caption,
        youtube_caption: post.youtube_caption,
        destinations: accounts
            .into_iter()
            .map(|account| Destination {
                provider: account.provider,
                account_id: account.provider_account_id,
            })
            .collect(),
    }))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SnapshotError {
    #[error("the scheduled Post could not be read")]
    Post(#[from] store::posts::PostError),
    #[error("the Brand's social accounts could not be read")]
    Accounts(#[from] store::social_accounts::SocialAccountError),
}

#[restate_sdk::workflow(name = "SocialPublisher")]
impl SocialPublisherService {
    #[restate_sdk::handler]
    #[allow(clippy::too_many_lines)]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(request): Json<SocialPublishRequest>,
    ) -> Result<Json<SocialPublishReport>, HandlerError> {
        let delay = (request.publish_at - Utc::now())
            .to_std()
            .unwrap_or(Duration::ZERO);
        if !delay.is_zero() {
            ctx.sleep(delay).await.map_err(HandlerError::from)?;
        }

        let post_id = request.post_id;
        let requested_at = request.publish_at;
        let surreal = self.publisher.surreal.clone();
        let snapshot = ctx
            .run(move || async move {
                scheduled_post_snapshot(&surreal, post_id, requested_at)
                    .await
                    .map(Json)
                    .map_err(HandlerError::from)
            })
            .name("load-scheduled-post")
            .await?
            .into_inner();
        let Some(snapshot) = snapshot else {
            return Ok(Json(SocialPublishReport {
                post_id,
                outcome: "skipped_stale_schedule".into(),
                destinations: 0,
            }));
        };

        let mut outcomes = Vec::with_capacity(snapshot.destinations.len());
        for destination in snapshot.destinations.clone() {
            let name = provider_name(destination.provider);
            let mut outcome = self
                .publish_one(&ctx, post_id, &snapshot, &destination, name)
                .await?;
            for _ in 1..RETRYABLE_ATTEMPTS {
                if !outcome.retryable {
                    break;
                }
                ctx.sleep(RETRY_AFTER).await.map_err(HandlerError::from)?;
                outcome = self
                    .publish_one(&ctx, post_id, &snapshot, &destination, name)
                    .await?;
            }
            if let Some(receipt) = outcome.link_pending.clone() {
                self.read_back_permalink(&ctx, post_id, &snapshot, &destination, &receipt, name)
                    .await?;
            }
            outcomes.push(outcome);
        }
        let failed = outcomes.iter().any(|outcome| outcome.state == FAILED);
        let unknown = outcomes.iter().any(|outcome| outcome.state == UNKNOWN);
        if !failed && !unknown {
            let surreal = self.publisher.surreal.clone();
            ctx.run(move || async move {
                surreal
                    .query("UPDATE $id SET status = 'published', updated_at = time::now() WHERE status = 'scheduled'")
                    .bind(("id", store::surreal::record_id("post", post_id)))
                    .await
                    .map_err(HandlerError::from)?
                    .check()
                    .map_err(HandlerError::from)?;
                store::postgres_mirror::after_write(
                    &surreal,
                    store::postgres_mirror::MirrorKey::Post(post_id),
                )
                .await;
                Ok(())
            })
            .name("publish-blog-article-without-social-destinations")
            .await?;
        }
        Ok(Json(SocialPublishReport {
            post_id,
            outcome: publication_outcome(&outcomes).into(),
            destinations: outcomes.len(),
        }))
    }
}

/// Attempts for a destination the provider rate-limited or failed temporarily.
pub(crate) const RETRYABLE_ATTEMPTS: usize = 3;
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(120);
/// How often a `TikTok` upload task is polled while the provider processes it.
pub(crate) const TIKTOK_POLL: Duration = Duration::from_secs(180);
/// A provider can take a moment to expose a new post's public link.
pub(crate) const PERMALINK_READS: usize = 3;
pub(crate) const PERMALINK_READ_AFTER: Duration = Duration::from_secs(30);

impl SocialPublisherService {
    /// Read a confirmed post's public link back from the provider. A link that
    /// stays unreadable leaves the publication confirmed with its link
    /// pending; the post is never sent again for it.
    async fn read_back_permalink(
        &self,
        ctx: &WorkflowContext<'_>,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
        receipt: &str,
        name: &str,
    ) -> Result<(), HandlerError> {
        for read in 0..PERMALINK_READS {
            if read > 0 {
                ctx.sleep(PERMALINK_READ_AFTER)
                    .await
                    .map_err(HandlerError::from)?;
            }
            let publisher = self.publisher.clone();
            let (snapshot, target, receipt) =
                (post.clone(), destination.clone(), receipt.to_string());
            let recorded = ctx
                .run(move || async move {
                    publisher
                        .read_permalink(post_id, &snapshot.brand_key, &target, &receipt)
                        .await
                        .map(Json)
                })
                .name(format!("permalink-{name}"))
                .await?
                .into_inner();
            if recorded {
                break;
            }
        }
        Ok(())
    }

    async fn publish_tiktok(
        &self,
        ctx: &WorkflowContext<'_>,
        post_id: Uuid,
        post: &PostSnapshot,
        target: &Destination,
    ) -> Result<PublishOutcome, HandlerError> {
        let publisher = self.publisher.clone();
        let (post, target) = (post.clone(), target.clone());
        let (snapshot, destination) = (post.clone(), target.clone());
        let accepted = ctx
            .run(move || async move {
                publisher
                    .start_tiktok(post_id, &snapshot, &destination)
                    .await
                    .map(Json)
            })
            .name("submit-tiktok-business-video")
            .await?
            .into_inner();
        let task = match accepted {
            Step::Continue(task) => task,
            Step::Settled(outcome) => return Ok(outcome),
        };
        for check in 0_u64.. {
            ctx.sleep(TIKTOK_POLL).await.map_err(HandlerError::from)?;
            let publisher = self.publisher.clone();
            let (snapshot, destination, task) = (post.clone(), target.clone(), task.clone());
            let polled = ctx
                .run(move || async move {
                    publisher
                        .poll_tiktok(post_id, &snapshot, &destination, &task)
                        .await
                        .map(Json)
                })
                .name(format!("poll-tiktok-business-video-{check}"))
                .await?
                .into_inner();
            if let Step::Settled(outcome) = polled {
                return Ok(outcome);
            }
        }
        Err(TerminalError::new("TikTok polling counter exhausted").into())
    }

    /// One destination's journaled steps. A container created by a journaled
    /// step is never created again when the invocation resumes.
    async fn prepare_destination(
        &self,
        ctx: &WorkflowContext<'_>,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
        name: &str,
    ) -> Result<Step<()>, HandlerError> {
        let publisher = self.publisher.clone();
        let (snapshot, target) = (post.clone(), destination.clone());
        Ok(ctx
            .run(move || async move {
                publisher
                    .prepare(post_id, &snapshot, &target)
                    .await
                    .map(Json)
            })
            .name(format!("prepare-{name}"))
            .await?
            .into_inner())
    }

    async fn publish_one(
        &self,
        ctx: &WorkflowContext<'_>,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
        name: &str,
    ) -> Result<PublishOutcome, HandlerError> {
        let prepared = self
            .prepare_destination(ctx, post_id, post, destination, name)
            .await?;
        if let Step::Settled(outcome) = prepared {
            return Ok(outcome);
        }
        if destination.provider == SocialProvider::Tiktok {
            return self.publish_tiktok(ctx, post_id, post, destination).await;
        }
        let Some(policy) = container_policy(destination.provider) else {
            let publisher = self.publisher.clone();
            let (snapshot, target) = (post.clone(), destination.clone());
            return Ok(ctx
                .run(move || async move {
                    publisher
                        .send_direct(post_id, &snapshot, &target)
                        .await
                        .map(Json)
                })
                .name(format!("publish-{name}"))
                .await?
                .into_inner());
        };

        let publisher = self.publisher.clone();
        let (snapshot, target) = (post.clone(), destination.clone());
        let created = ctx
            .run(move || async move {
                publisher
                    .create_container(post_id, &snapshot, &target)
                    .await
                    .map(Json)
            })
            .name(format!("create-container-{name}"))
            .await?
            .into_inner();
        let container = match created {
            Step::Continue(container) => container,
            Step::Settled(outcome) => return Ok(outcome),
        };
        for check in 0..policy.checks {
            ctx.sleep(policy.interval)
                .await
                .map_err(HandlerError::from)?;
            let publisher = self.publisher.clone();
            let (snapshot, target, id) = (post.clone(), destination.clone(), container.clone());
            let status = ctx
                .run(move || async move {
                    publisher
                        .container_status(post_id, &snapshot, &target, &id)
                        .await
                        .map(Json)
                })
                .name(format!("container-status-{name}-{check}"))
                .await?
                .into_inner();
            match status {
                Step::Settled(outcome) => return Ok(outcome),
                Step::Continue(ContainerStatus::Processing) => {}
                Step::Continue(ContainerStatus::Ready) => {
                    let publisher = self.publisher.clone();
                    let (snapshot, target, id) =
                        (post.clone(), destination.clone(), container.clone());
                    return Ok(ctx
                        .run(move || async move {
                            publisher
                                .publish_container(post_id, &snapshot, &target, &id)
                                .await
                                .map(Json)
                        })
                        .name(format!("publish-container-{name}"))
                        .await?
                        .into_inner());
                }
            }
        }
        let publisher = self.publisher.clone();
        let provider = destination.provider;
        Ok(ctx
            .run(move || async move {
                publisher
                    .fail(
                        post_id,
                        provider,
                        "The provider was still processing the media; nothing was published",
                    )
                    .await
                    .map(Json)
            })
            .name(format!("container-timeout-{name}"))
            .await?
            .into_inner())
    }
}

pub(crate) const fn provider_name(provider: SocialProvider) -> &'static str {
    provider.as_str()
}

/// Everything a destination step needs. Steps receive no secret through the
/// journal: each resolves its own credential immediately before its request.
#[derive(Clone)]
pub struct Publisher {
    surreal: SurrealDb,
    kms: Arc<dyn cloud::RuntimeKms>,
    storage: Arc<dyn cloud::StorageService>,
    egress: SocialEgressGate,
    refresh: TokenRefreshConfig,
    endpoints: ProviderEndpoints,
    http: reqwest::Client,
    /// The public address a provider fetches a video Asset from.
    video_url: fn(&str) -> String,
}

impl Publisher {
    /// Assembles a publisher from its live dependencies with the default
    /// provider endpoints. The Restate services and the DBOS twin drive the
    /// same instance of this type; tests override the endpoints afterwards
    /// with `set_endpoints`.
    #[must_use]
    pub fn with_parts(
        surreal: SurrealDb,
        kms: Arc<dyn cloud::RuntimeKms>,
        storage: Arc<dyn cloud::StorageService>,
        egress: SocialEgressGate,
        refresh: TokenRefreshConfig,
        http: reqwest::Client,
        video_url: fn(&str) -> String,
    ) -> Self {
        Self {
            surreal,
            kms,
            storage,
            egress,
            refresh,
            endpoints: ProviderEndpoints::default(),
            http,
            video_url,
        }
    }

    /// Override the social egress gate. The DBOS twin's integration tests run
    /// without the deployment's egress environment (which denies by default),
    /// so they supply the same permissive gate the Restate tests construct
    /// directly.
    #[must_use]
    pub fn with_egress(mut self, egress: SocialEgressGate) -> Self {
        self.egress = egress;
        self
    }

    /// Override the public address a provider fetches a video Asset from.
    /// The Restate in-file tests build their publisher directly with a
    /// synthetic origin; the DBOS twin's integration tests reach the same
    /// override through this builder.
    #[must_use]
    pub fn with_video_url(mut self, video_url: fn(&str) -> String) -> Self {
        self.video_url = video_url;
        self
    }

    /// Point every provider endpoint at one base URL. Tests and the DBOS
    /// twin's checks drive all providers through a single mock origin.
    #[must_use]
    pub fn with_provider_base(mut self, base: &str) -> Self {
        self.endpoints = ProviderEndpoints {
            facebook_graph: base.to_owned(),
            threads: base.to_owned(),
            youtube_upload: base.to_owned(),
            youtube_api: base.to_owned(),
            x_api: base.to_owned(),
            tiktok: format!("{base}/tiktok"),
            linkedin_rest: base.to_owned(),
        };
        self
    }

    /// The store the publication state is admitted into and claimed from.
    #[must_use]
    pub(crate) fn surreal(&self) -> &SurrealDb {
        &self.surreal
    }

    pub(crate) async fn start_tiktok(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<Step<String>, HandlerError> {
        let publications = store::social_publications::for_posts(&self.surreal, &[post_id])
            .await
            .map_err(HandlerError::from)?;
        if let Some(task) = publications
            .iter()
            .find(|p| p.provider == "tiktok")
            .and_then(|p| p.provider_task_id.clone())
        {
            return Ok(Step::Continue(task));
        }
        let ai_generated = match self
            .tiktok_preflight_source(post_id, post, destination)
            .await?
        {
            Step::Continue(disclosure) => disclosure,
            Step::Settled(outcome) => return Ok(Step::Settled(outcome)),
        };
        let Some(key) = &post.video_asset else {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "TikTok requires portrait video",
                )
                .await
                .map(Step::Settled);
        };
        let media = match self.tiktok_media(post_id, post, destination, key).await? {
            Step::Continue(media) => media,
            Step::Settled(outcome) => return Ok(Step::Settled(outcome)),
        };
        let credential = match self.writable(post_id, post, destination).await? {
            Ok(token) => token,
            Err(outcome) => return Ok(Step::Settled(outcome)),
        };
        let maximum = match self
            .tiktok_settings(post_id, destination, &credential)
            .await?
        {
            Step::Continue(maximum) => maximum,
            Step::Settled(outcome) => return Ok(Step::Settled(outcome)),
        };
        if let Err(problem) = crate::tiktok_business::video_duration(&media, maximum) {
            return self
                .fail_before_send(post_id, destination.provider, problem)
                .await
                .map(Step::Settled);
        }
        let caption = caption(post, destination.provider);
        let video_url = (self.video_url)(key);
        // Existing public Asset URLs have no expiry. Signed URLs are not accepted.
        if url::Url::parse(&video_url)
            .ok()
            .is_none_or(|url| url.query().is_some())
        {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "TikTok requires a verified permanent public Asset URL",
                )
                .await
                .map(Step::Settled);
        }
        if !self.tiktok_property_verified(&video_url).await {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "TikTok app has no verified URL property covering this Asset URL",
                )
                .await
                .map(Step::Settled);
        }
        let request = match crate::tiktok_business::VideoRequest::new(
            &destination.account_id,
            &video_url,
            &caption,
            ai_generated,
        ) {
            Ok(request) => request,
            Err(problem) => {
                return self
                    .fail_before_send(post_id, destination.provider, problem)
                    .await
                    .map(Step::Settled)
            }
        };
        let credential = match self
            .tiktok_final_credential(post_id, post, destination, &credential)
            .await?
        {
            Step::Continue(credential) => credential,
            Step::Settled(outcome) => return Ok(Step::Settled(outcome)),
        };
        self.submit_tiktok(post_id, destination, &credential, &request)
            .await
    }

    async fn tiktok_preflight_source(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<Step<bool>, HandlerError> {
        let Some(ai_generated) = post.tiktok_ai_generated else {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "Declare tiktok_ai_generated: true or false before TikTok publishing",
                )
                .await
                .map(Step::Settled);
        };
        if !self.tiktok_source_matches(post_id, post).await? {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "The approved TikTok source changed; review the new schedule",
                )
                .await
                .map(Step::Settled);
        }
        Ok(Step::Continue(ai_generated))
    }

    async fn tiktok_final_credential(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
        preflight: &SendCredential,
    ) -> Result<Step<SendCredential>, HandlerError> {
        // Re-read the Brand authorization and production guard immediately before the write.
        let preflight_access_token = preflight.access_token();
        let credential = match self.writable(post_id, post, destination).await? {
            Ok(token) => token,
            Err(outcome) => return Ok(Step::Settled(outcome)),
        };
        if !self.tiktok_source_matches(post_id, post).await? {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "The approved TikTok source changed during preflight",
                )
                .await
                .map(Step::Settled);
        }
        if credential.access_token() != preflight_access_token {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "TikTok authorization changed during preflight; retry with current settings",
                )
                .await
                .map(Step::Settled);
        }
        Ok(Step::Continue(credential))
    }

    async fn tiktok_media(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
        key: &str,
    ) -> Result<Step<Vec<u8>>, HandlerError> {
        let measured = store::posts::find_video(&self.surreal, key)
            .await
            .map_err(HandlerError::from)?;
        if !post.vertical_video
            || measured.as_ref().is_none_or(|video| {
                video.brand_key != post.brand_key
                    || video.orientation != "vertical"
                    || video.width < 360
                    || video.height < 360
            })
        {
            return self.fail_before_send(post_id,destination.provider,"TikTok requires this Brand's measured portrait video at least 360 pixels on each side").await.map(Step::Settled);
        }
        let media =
            match self.storage.get(key).await {
                Ok(media) if media.bytes.len() <= 64 * 1024 * 1024 => media,
                _ => return self
                    .fail_before_send(
                        post_id,
                        destination.provider,
                        "TikTok media is unavailable or exceeds Navigator's 64 MiB preflight limit",
                    )
                    .await
                    .map(Step::Settled),
            };
        Ok(Step::Continue(media.bytes))
    }

    async fn tiktok_settings(
        &self,
        post_id: Uuid,
        destination: &Destination,
        credential: &SendCredential,
    ) -> Result<Step<u32>, HandlerError> {
        let settings = tiktok_json(
            self.http
                .get(format!(
                    "{}/business/video/settings/",
                    self.endpoints.tiktok
                ))
                .header("Access-Token", credential.access_token())
                .query(&[("business_id", destination.account_id.as_str())])
                .send()
                .await,
        )
        .await;
        let Some(settings) = settings else {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "TikTok account settings could not be verified",
                )
                .await
                .map(Step::Settled);
        };
        if !settings
            .get("privacy_level_options")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|options| {
                options
                    .iter()
                    .any(|option| option.as_str() == Some("PUBLIC_TO_EVERYONE"))
            })
        {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "TikTok account is not eligible for public posts",
                )
                .await
                .map(Step::Settled);
        }
        let Some(maximum) = settings
            .get("max_video_post_duration_sec")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
        else {
            return self
                .fail_before_send(
                    post_id,
                    destination.provider,
                    "TikTok maximum video duration could not be verified",
                )
                .await
                .map(Step::Settled);
        };
        Ok(Step::Continue(maximum))
    }

    async fn tiktok_property_verified(&self, video_url: &str) -> bool {
        let Some((app_id, app_secret)) = self.refresh.tiktok_client.as_ref() else {
            return false;
        };
        // Official read-only endpoint: never log its secret-bearing URL or raw errors.
        let properties = tiktok_json(
            self.http
                .get(format!("{}/business/property/list/", self.endpoints.tiktok))
                .query(&[("app_id", app_id), ("secret", app_secret)])
                .send()
                .await,
        )
        .await;
        properties.as_ref().is_some_and(|properties| {
            crate::tiktok_business::verified_property(properties, video_url)
        })
    }

    async fn submit_tiktok(
        &self,
        post_id: Uuid,
        destination: &Destination,
        credential: &SendCredential,
        request: &crate::tiktok_business::VideoRequest<'_>,
    ) -> Result<Step<String>, HandlerError> {
        if let Some(outcome) = self.claim_write(post_id, destination.provider).await? {
            return Ok(Step::Settled(outcome));
        }
        let reply = tiktok_json(
            self.http
                .post(format!("{}/business/video/publish/", self.endpoints.tiktok))
                .header("Access-Token", credential.access_token())
                .json(&request)
                .send()
                .await,
        )
        .await;
        let Some(task) = reply
            .as_ref()
            .and_then(|body| body.get("share_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty() && id.len() <= 256)
        else {
            return self
                .unknown(
                    post_id,
                    destination.provider,
                    "TikTok publish outcome is unknown; do not send again",
                )
                .await
                .map(Step::Settled);
        };
        store::social_publications::record_provider_task(&self.surreal, post_id, "tiktok", task)
            .await
            .map_err(HandlerError::from)?;
        Ok(Step::Continue(task.to_string()))
    }

    async fn tiktok_source_matches(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
    ) -> Result<bool, HandlerError> {
        let current = store::posts::find_by_id(&self.surreal, post_id)
            .await
            .map_err(HandlerError::from)?;
        Ok(current.is_some_and(|row| {
            row.status == "scheduled"
                && row.publish_at == post.publish_at
                && row.video_asset == post.video_asset
                && row.vertical_video
                && row.tiktok_ai_generated == post.tiktok_ai_generated
                && row.description == post.description
        }))
    }

    async fn tiktok_terminal_outcome(
        &self,
        post_id: Uuid,
        destination: &Destination,
        task: &str,
    ) -> Result<Option<PublishOutcome>, HandlerError> {
        let publication = store::social_publications::for_posts(&self.surreal, &[post_id])
            .await
            .map_err(HandlerError::from)?
            .into_iter()
            .find(|publication| publication.provider == "tiktok");
        if let Some(publication) = publication {
            if publication.provider_task_id.as_deref() != Some(task) {
                return Ok(Some(PublishOutcome::new(destination.provider, UNKNOWN)));
            }
            if matches!(publication.state.as_str(), CONFIRMED | FAILED) {
                let mut outcome = PublishOutcome::new(destination.provider, &publication.state);
                if publication.state == CONFIRMED && publication.permalink.is_none() {
                    outcome.link_pending = publication.provider_post_id;
                }
                return Ok(Some(outcome));
            }
        }
        Ok(None)
    }

    pub(crate) async fn poll_tiktok(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
        task: &str,
    ) -> Result<Step<()>, HandlerError> {
        if let Some(outcome) = self
            .tiktok_terminal_outcome(post_id, destination, task)
            .await?
        {
            return Ok(Step::Settled(outcome));
        }
        if self.egress.check().is_err() {
            return self
                .unknown(
                    post_id,
                    destination.provider,
                    "TikTok task retained; this deployment may not contact the provider",
                )
                .await
                .map(Step::Settled);
        }
        let Ok(credential) = send_credential(
            &self.surreal,
            self.kms.as_ref(),
            &self.http,
            &self.refresh,
            &post.brand_key,
            destination.provider,
            &destination.account_id,
        )
        .await
        else {
            return self
                .unknown(
                    post_id,
                    destination.provider,
                    "Reconnect TikTok to reconcile the retained task",
                )
                .await
                .map(Step::Settled);
        };
        let Some(status) = self
            .tiktok_task_status(destination, task, &credential)
            .await
        else {
            return Ok(Step::Continue(()));
        };
        match status {
            Ok(crate::tiktok_business::PublishStatus::Complete(ids)) if ids.len() == 1 => {
                store::social_publications::mark_confirmed(
                    &self.surreal,
                    post_id,
                    "tiktok",
                    &ids[0],
                    None,
                )
                .await
                .map_err(HandlerError::from)?;
                let mut outcome = PublishOutcome::new(destination.provider, CONFIRMED);
                outcome.link_pending = Some(ids[0].clone());
                Ok(Step::Settled(outcome))
            }
            Ok(crate::tiktok_business::PublishStatus::Failed) => self
                .fail(
                    post_id,
                    destination.provider,
                    "TikTok reported a terminal media processing failure",
                )
                .await
                .map(Step::Settled),
            Ok(crate::tiktok_business::PublishStatus::Draft) => self
                .unknown(
                    post_id,
                    destination.provider,
                    "TikTok returned an inbox draft; no public post was confirmed",
                )
                .await
                .map(Step::Settled),
            Ok(crate::tiktok_business::PublishStatus::Processing) | Err(_) => {
                Ok(Step::Continue(()))
            }
            Ok(crate::tiktok_business::PublishStatus::Complete(_)) => self
                .unknown(
                    post_id,
                    destination.provider,
                    "TikTok returned an unexpected public post identity set",
                )
                .await
                .map(Step::Settled),
        }
    }

    /// Admit the destination and check, before anything is sent, that this
    /// deployment may publish, the account is usable, and the post carries
    /// media the provider accepts.
    async fn tiktok_task_status(
        &self,
        destination: &Destination,
        task: &str,
        credential: &SendCredential,
    ) -> Option<Result<crate::tiktok_business::PublishStatus, &'static str>> {
        let response = self
            .http
            .get(format!(
                "{}/business/publish/status/",
                self.endpoints.tiktok
            ))
            .header("Access-Token", credential.access_token())
            .query(&[
                ("business_id", destination.account_id.as_str()),
                ("publish_id", task),
            ])
            .send()
            .await;
        let Ok(response) = response else {
            return None;
        };
        if !response.status().is_success() {
            return None;
        }
        let Ok(bytes) = response.bytes().await else {
            return None;
        };
        Some(crate::tiktok_business::publish_status(&bytes))
    }

    pub(crate) async fn prepare(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<Step<()>, HandlerError> {
        let provider = destination.provider;
        let admission =
            store::social_publications::admit(&self.surreal, post_id, provider.as_str())
                .await
                .map_err(HandlerError::from)?;
        match admission {
            store::social_publications::Admission::AlreadyConfirmed(publication) => {
                let mut outcome = PublishOutcome::new(provider, CONFIRMED);
                if publication.permalink.is_none() {
                    outcome.link_pending = publication.provider_post_id;
                }
                return Ok(Step::Settled(outcome));
            }
            store::social_publications::Admission::ReconciliationRequired(publication) => {
                if provider == SocialProvider::Tiktok && publication.provider_task_id.is_some() {
                    return Ok(Step::Continue(()));
                }
                return Ok(Step::Settled(PublishOutcome::new(provider, UNKNOWN)));
            }
            // `sending` means no provider write has been claimed yet, so a
            // resumed invocation continues where it stopped.
            store::social_publications::Admission::AlreadyInFlight(_)
            | store::social_publications::Admission::Admitted(_) => {}
        }
        if provider == SocialProvider::Tiktok && post.tiktok_ai_generated.is_none() {
            return self
                .fail_before_send(
                    post_id,
                    provider,
                    "Declare tiktok_ai_generated: true or false before TikTok publishing",
                )
                .await
                .map(Step::Settled);
        }
        if let Err(outcome) = self.writable(post_id, post, destination).await? {
            return Ok(Step::Settled(outcome));
        }
        if provider == SocialProvider::X
            && (post.video_asset.is_some()
                || caption(post, provider).is_empty()
                || !x_text_eligible(&caption(post, provider)))
        {
            return self.fail_before_send(post_id, provider,
                "X supports text only within its conservative 280-weight budget; shorten the Post description")
                .await.map(Step::Settled);
        }
        if provider == SocialProvider::Linkedin
            && (post.video_asset.is_some()
                || caption(post, provider).chars().count() > 3000
                || destination.account_id.is_empty()
                || !destination
                    .account_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit()))
        {
            return self.fail_before_send(post_id, provider,
                "LinkedIn requires a numeric Company Page ID and text of at most 3000 characters without video")
                .await.map(Step::Settled);
        }
        let requires_video = matches!(
            provider,
            SocialProvider::Instagram | SocialProvider::Youtube
        );
        let media_problem = match &post.video_asset {
            None if requires_video => Some(format!(
                "{} publishes video only; this post has no video",
                provider_label(provider)
            )),
            Some(key) if container_policy(provider).is_some() => {
                if self.storage.get(key).await.is_err() {
                    Some("The post's video is unavailable".to_string())
                } else if !(self.video_url)(key).starts_with("https://") {
                    Some("The post's video has no public HTTPS address".to_string())
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(problem) = media_problem {
            return self
                .fail_before_send(post_id, provider, &problem)
                .await
                .map(Step::Settled);
        }
        Ok(Step::Continue(()))
    }

    /// The egress gate and the account, checked immediately before a provider
    /// request. A refusal is recorded and returned as the destination outcome.
    async fn writable(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<Result<SendCredential, PublishOutcome>, HandlerError> {
        let provider = destination.provider;
        if let Err(denied) = self.egress.check() {
            self.record_before_send(post_id, provider, &denied.to_string())
                .await?;
            return Ok(Err(PublishOutcome::new(provider, BLOCKED)));
        }
        match send_credential(
            &self.surreal,
            self.kms.as_ref(),
            &self.http,
            &self.refresh,
            &post.brand_key,
            provider,
            &destination.account_id,
        )
        .await
        {
            Ok(credential) => Ok(Ok(credential)),
            Err(failure) => self
                .fail_before_send(post_id, provider, &failure.to_string())
                .await
                .map(Err),
        }
    }

    async fn record_before_send(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        reason: &str,
    ) -> Result<(), HandlerError> {
        store::social_publications::mark_failed_before_send(
            &self.surreal,
            post_id,
            provider.as_str(),
            reason,
        )
        .await
        .map(|_| ())
        .map_err(HandlerError::from)
    }

    async fn fail_before_send(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        reason: &str,
    ) -> Result<PublishOutcome, HandlerError> {
        self.record_before_send(post_id, provider, reason).await?;
        Ok(PublishOutcome::new(provider, FAILED))
    }

    /// Record a known failure: the provider refused, or nothing was published.
    pub(crate) async fn fail(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        reason: &str,
    ) -> Result<PublishOutcome, HandlerError> {
        store::social_publications::mark_failed(&self.surreal, post_id, provider.as_str(), reason)
            .await
            .map_err(HandlerError::from)?;
        Ok(PublishOutcome::new(provider, FAILED))
    }

    async fn unknown(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        reason: &str,
    ) -> Result<PublishOutcome, HandlerError> {
        store::social_publications::mark_unknown(&self.surreal, post_id, provider.as_str(), reason)
            .await
            .map_err(HandlerError::from)?;
        Ok(PublishOutcome::new(provider, UNKNOWN))
    }

    /// Claim the publication's one public write. `None` means it was already
    /// claimed and the destination needs reconciliation, not another send.
    async fn claim_write(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
    ) -> Result<Option<PublishOutcome>, HandlerError> {
        let claimed = store::social_publications::begin_provider_write(
            &self.surreal,
            post_id,
            provider.as_str(),
        )
        .await
        .map_err(HandlerError::from)?;
        Ok((!claimed).then(|| PublishOutcome::new(provider, UNKNOWN)))
    }

    /// Create an Instagram Reel or Threads media container. A container is not
    /// public, so any failure here is known: nothing was published.
    pub(crate) async fn create_container(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<Step<String>, HandlerError> {
        let provider = destination.provider;
        let credential = match self.writable(post_id, post, destination).await? {
            Ok(credential) => credential,
            Err(outcome) => return Ok(Step::Settled(outcome)),
        };
        let caption = caption(post, provider);
        let video_url = post.video_asset.as_deref().map(self.video_url);
        let token = credential.access_token();
        let response = match provider {
            SocialProvider::Instagram => {
                let Some(video_url) = video_url.as_deref() else {
                    return self
                        .fail(post_id, provider, "Instagram publishes video only")
                        .await
                        .map(Step::Settled);
                };
                self.http
                    .post(format!(
                        "{}/{}/media",
                        self.endpoints.facebook_graph, destination.account_id
                    ))
                    .form(&[
                        ("media_type", "REELS"),
                        ("video_url", video_url),
                        ("caption", caption.as_str()),
                        ("access_token", token),
                    ])
                    .send()
                    .await
            }
            SocialProvider::Threads => {
                let mut form = vec![("text", caption.as_str()), ("access_token", token)];
                match video_url.as_deref() {
                    Some(video_url) => {
                        form.extend([("media_type", "VIDEO"), ("video_url", video_url)]);
                    }
                    None => form.push(("media_type", "TEXT")),
                }
                self.http
                    .post(format!(
                        "{}/{}/threads",
                        self.endpoints.threads, destination.account_id
                    ))
                    .form(&form)
                    .send()
                    .await
            }
            SocialProvider::Facebook
            | SocialProvider::Youtube
            | SocialProvider::Linkedin
            | SocialProvider::X
            | SocialProvider::Tiktok => {
                return self
                    .fail(
                        post_id,
                        provider,
                        "This provider does not publish through a media container",
                    )
                    .await
                    .map(Step::Settled);
            }
        };
        match read_reply(response).await {
            ProviderReply::Accepted(body) => {
                match body.get("id").and_then(serde_json::Value::as_str) {
                    Some(id) => Ok(Step::Continue(id.to_string())),
                    None => self
                        .fail(
                            post_id,
                            provider,
                            "The media container could not be created; nothing was published",
                        )
                        .await
                        .map(Step::Settled),
                }
            }
            ProviderReply::Refused(refusal) => self
                .refused(post_id, provider, &refusal)
                .await
                .map(Step::Settled),
            ProviderReply::Ambiguous => self
                .fail(
                    post_id,
                    provider,
                    "The media container could not be created; nothing was published",
                )
                .await
                .map(Step::Settled),
        }
    }

    /// Read a container's readiness. An unreadable status is retried at the
    /// next check; an expired or failed container is a known failure.
    pub(crate) async fn container_status(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
        container: &str,
    ) -> Result<Step<ContainerStatus>, HandlerError> {
        let provider = destination.provider;
        let credential = match self.writable(post_id, post, destination).await? {
            Ok(credential) => credential,
            Err(outcome) => return Ok(Step::Settled(outcome)),
        };
        let (base, fields, field) = match provider {
            SocialProvider::Instagram => {
                (&self.endpoints.facebook_graph, "status_code", "status_code")
            }
            _ => (&self.endpoints.threads, "status,error_message", "status"),
        };
        let response = self
            .http
            .get(format!("{base}/{container}"))
            .query(&[
                ("fields", fields),
                ("access_token", credential.access_token()),
            ])
            .send()
            .await;
        let ProviderReply::Accepted(body) = read_reply(response).await else {
            return Ok(Step::Continue(ContainerStatus::Processing));
        };
        match body.get(field).and_then(serde_json::Value::as_str) {
            Some("FINISHED") => Ok(Step::Continue(ContainerStatus::Ready)),
            Some("ERROR" | "EXPIRED") => {
                let detail = body
                    .get("error_message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the provider could not process the media");
                self.fail(
                    post_id,
                    provider,
                    &bounded(&format!("The media container failed: {detail}")),
                )
                .await
                .map(Step::Settled)
            }
            Some("PUBLISHED") => self
                .unknown(
                    post_id,
                    provider,
                    "The media container was already published",
                )
                .await
                .map(Step::Settled),
            _ => Ok(Step::Continue(ContainerStatus::Processing)),
        }
    }

    /// Publish a ready container: the destination's one public write.
    pub(crate) async fn publish_container(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
        container: &str,
    ) -> Result<PublishOutcome, HandlerError> {
        let provider = destination.provider;
        let credential = match self.writable(post_id, post, destination).await? {
            Ok(credential) => credential,
            Err(outcome) => return Ok(outcome),
        };
        if let Some(outcome) = self.claim_write(post_id, provider).await? {
            return Ok(outcome);
        }
        let (base, edge) = match provider {
            SocialProvider::Instagram => (&self.endpoints.facebook_graph, "media_publish"),
            _ => (&self.endpoints.threads, "threads_publish"),
        };
        let response = self
            .http
            .post(format!("{base}/{}/{edge}", destination.account_id))
            .form(&[
                ("creation_id", container),
                ("access_token", credential.access_token()),
            ])
            .send()
            .await;
        self.settle(post_id, provider, read_reply(response).await)
            .await
    }

    /// Destinations that publish in one write: [`SocialProvider::Facebook`]
    /// and [`SocialProvider::Youtube`].
    pub(crate) async fn send_direct(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<PublishOutcome, HandlerError> {
        let provider = destination.provider;
        if provider == SocialProvider::X {
            return self.send_x_destination(post_id, post, destination).await;
        }
        let media = match &post.video_asset {
            Some(key) => match self.storage.get(key).await {
                Ok(object) => Some(object),
                Err(_) => {
                    return self
                        .fail_before_send(post_id, provider, "The post's video is unavailable")
                        .await;
                }
            },
            None => None,
        };
        let credential = match self.writable(post_id, post, destination).await? {
            Ok(credential) => credential,
            Err(outcome) => return Ok(outcome),
        };
        if let Some(outcome) = self.claim_write(post_id, provider).await? {
            return Ok(outcome);
        }
        let caption = caption(post, provider);
        let reply = if provider == SocialProvider::Linkedin {
            self.send_linkedin(&destination.account_id, &caption, credential.access_token())
                .await
        } else if provider == SocialProvider::Youtube {
            let Some(media) = media.as_ref() else {
                return self
                    .fail(post_id, provider, "YouTube publishes video only")
                    .await;
            };
            self.upload_youtube(post, &caption, credential.access_token(), media)
                .await
        } else {
            let base = format!(
                "{}/{}",
                self.endpoints.facebook_graph, destination.account_id
            );
            let token = credential.access_token();
            let response = match media.as_ref() {
                Some(media) => {
                    let Ok(part) = reqwest::multipart::Part::bytes(media.bytes.clone())
                        .file_name("video.mp4")
                        .mime_str(&media.content_type)
                    else {
                        return self
                            .fail(post_id, provider, "The post's video type is not supported")
                            .await;
                    };
                    self.http
                        .post(format!("{base}/videos"))
                        .multipart(
                            reqwest::multipart::Form::new()
                                .text("description", caption.clone())
                                .text("access_token", token.to_string())
                                .part("source", part),
                        )
                        .send()
                        .await
                }
                None => {
                    self.http
                        .post(format!("{base}/feed"))
                        .form(&[("message", caption.as_str()), ("access_token", token)])
                        .send()
                        .await
                }
            };
            read_reply(response).await
        };
        self.settle(post_id, provider, reply).await
    }

    /// X verifies identity, then checks revocation again before claiming the write.
    async fn send_x_destination(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<PublishOutcome, HandlerError> {
        let provider = SocialProvider::X;
        let credential = match self.writable(post_id, post, destination).await? {
            Ok(credential) => credential,
            Err(outcome) => return Ok(outcome),
        };
        if !self
            .verify_x_identity(&destination.account_id, credential.access_token())
            .await
        {
            return self
                .fail_before_send(
                    post_id,
                    provider,
                    "X account identity could not be verified; reconnect this Brand",
                )
                .await;
        }
        let current = match self.writable(post_id, post, destination).await? {
            Ok(current) if current.access_token() == credential.access_token() => current,
            Ok(_) => {
                return self
                    .fail_before_send(
                        post_id,
                        provider,
                        "X credentials changed during verification; retry without sending",
                    )
                    .await
            }
            Err(outcome) => return Ok(outcome),
        };
        if let Some(outcome) = self.claim_write(post_id, provider).await? {
            return Ok(outcome);
        }
        self.settle(
            post_id,
            provider,
            self.send_x(&caption(post, provider), current.access_token())
                .await,
        )
        .await
    }

    async fn verify_x_identity(&self, account_id: &str, token: &str) -> bool {
        let response = self
            .http
            .get(format!("{}/users/me", self.endpoints.x_api))
            .bearer_auth(token)
            .send()
            .await;
        let ProviderReply::Accepted(body) = read_reply(response).await else {
            return false;
        };
        body.pointer("/data/id").and_then(serde_json::Value::as_str) == Some(account_id)
    }

    /// X server failures cannot prove that a write did not happen.
    async fn send_x(&self, text: &str, token: &str) -> ProviderReply {
        let response = self
            .http
            .post(format!("{}/tweets", self.endpoints.x_api))
            .bearer_auth(token)
            .json(&serde_json::json!({"text": text}))
            .send()
            .await;
        let Ok(response) = response else {
            return ProviderReply::Ambiguous;
        };
        let status = response.status();
        if status.is_client_error() {
            return ProviderReply::Refused(ProviderRefusal {
                kind: refusal_kind(status, None),
                message: format!("HTTP {status}"),
            });
        }
        if status != reqwest::StatusCode::CREATED {
            return ProviderReply::Ambiguous;
        }
        match read_reply(Ok(response)).await {
            ProviderReply::Accepted(body) => match body
                .pointer("/data/id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| {
                    !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())
                }) {
                Some(id) => ProviderReply::Accepted(serde_json::json!({"id": id})),
                None => ProviderReply::Ambiguous,
            },
            _ => ProviderReply::Ambiguous,
        }
    }

    /// `LinkedIn` returns the receipt in a header and an empty response body.
    /// Missing receipts and server errors retain the existing unknown state.
    async fn send_linkedin(&self, account_id: &str, text: &str, token: &str) -> ProviderReply {
        let response = self
            .http
            .post(format!("{}/posts", self.endpoints.linkedin_rest))
            .bearer_auth(token)
            .header("LinkedIn-Version", "202609")
            .header("X-Restli-Protocol-Version", "2.0.0")
            .json(&serde_json::json!({
                "author": format!("urn:li:organization:{account_id}"),
                "commentary": text,
                "visibility": "PUBLIC",
                "distribution": {"feedDistribution": "MAIN_FEED", "targetEntities": [],
                    "thirdPartyDistributionChannels": []},
                "lifecycleState": "PUBLISHED", "isReshareDisabledByAuthor": false
            }))
            .send()
            .await;
        let Ok(response) = response else {
            return ProviderReply::Ambiguous;
        };
        if response.status() == reqwest::StatusCode::CREATED {
            let Some(id) = response
                .headers()
                .get("x-restli-id")
                .and_then(|header| header.to_str().ok())
                .filter(|id| id.starts_with("urn:li:share:") || id.starts_with("urn:li:ugcPost:"))
            else {
                return ProviderReply::Ambiguous;
            };
            return ProviderReply::Accepted(serde_json::json!({"id": id}));
        }
        if response.status().is_success() {
            return ProviderReply::Ambiguous;
        }
        read_reply(Ok(response)).await
    }

    async fn upload_youtube(
        &self,
        post: &PostSnapshot,
        caption: &str,
        token: &str,
        media: &cloud::StoredObject,
    ) -> ProviderReply {
        let start = self
            .http
            .post(format!("{}/videos", self.endpoints.youtube_upload))
            .query(&[("uploadType", "resumable"), ("part", "snippet,status")])
            .bearer_auth(token)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/json; charset=UTF-8",
            )
            .json(&serde_json::json!({
                "snippet": {"title": post.title, "description": caption, "categoryId": "22"},
                "status": {"privacyStatus": "public"}
            }))
            .send()
            .await;
        let response = match start {
            Ok(response) if response.status().is_success() => response,
            // No video exists until the upload completes, so a refused or
            // unreachable session start is a known failure.
            other => {
                return match read_reply(other).await {
                    ProviderReply::Refused(refusal) => ProviderReply::Refused(refusal),
                    _ => ProviderReply::Refused(ProviderRefusal::rejected(
                        "The YouTube upload session could not be started",
                    )),
                };
            }
        };
        let Some(upload_url) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
        else {
            return ProviderReply::Refused(ProviderRefusal::rejected(
                "The YouTube upload session had no upload address",
            ));
        };
        read_reply(
            self.http
                .put(upload_url)
                .header(reqwest::header::CONTENT_TYPE, &media.content_type)
                .body(media.bytes.clone())
                .send()
                .await,
        )
        .await
    }

    /// Record a provider's refusal: nothing was published.
    async fn refused(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        refusal: &ProviderRefusal,
    ) -> Result<PublishOutcome, HandlerError> {
        let mut outcome = self
            .fail(post_id, provider, &refusal.reason(provider))
            .await?;
        outcome.retryable = matches!(
            refusal.kind,
            RefusalKind::RateLimited | RefusalKind::Temporary
        );
        Ok(outcome)
    }

    /// Read the public link of a confirmed post through the provider's read
    /// endpoint and record it. Returns whether a link was recorded; a read
    /// that fails changes nothing and sends nothing.
    async fn read_tiktok_permalink(
        &self,
        post_id: Uuid,
        destination: &Destination,
        receipt: &str,
        credential: &SendCredential,
    ) -> Result<bool, HandlerError> {
        let filters = serde_json::json!({"video_ids":[receipt],"ad_post_only":false}).to_string();
        let body = tiktok_json(
            self.http
                .get(format!("{}/business/video/list/", self.endpoints.tiktok))
                .header("Access-Token", credential.access_token())
                .query(&[
                    ("business_id", destination.account_id.as_str()),
                    (
                        "fields",
                        "[\"item_id\",\"share_url\",\"media_type\",\"is_ad\"]",
                    ),
                    ("filters", filters.as_str()),
                ])
                .send()
                .await,
        )
        .await;
        let Some(video) = body
            .as_ref()
            .and_then(|body| body.get("videos"))
            .and_then(serde_json::Value::as_array)
            .and_then(|videos| {
                videos.iter().find(|video| {
                    video.get("item_id").and_then(serde_json::Value::as_str) == Some(receipt)
                })
            })
        else {
            return Ok(false);
        };
        if video.get("media_type").and_then(serde_json::Value::as_str) != Some("VIDEO")
            || video.get("is_ad").and_then(serde_json::Value::as_bool) != Some(false)
        {
            return Ok(false);
        }
        let Some(link) = video.get("share_url").and_then(serde_json::Value::as_str) else {
            return Ok(false);
        };
        let Ok(url) = url::Url::parse(link) else {
            return Ok(false);
        };
        if url.scheme() != "https"
            || !matches!(url.host_str(), Some("www.tiktok.com" | "tiktok.com"))
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Ok(false);
        }
        store::social_publications::record_permalink(&self.surreal, post_id, "tiktok", link)
            .await
            .map(|_| true)
            .map_err(HandlerError::from)
    }

    /// Revisit retained uploads after the initial polling window. This path only reads providers.
    pub(crate) async fn reconcile_youtube_links(&self) -> Result<usize, HandlerError> {
        if self.egress.check().is_err() {
            return Ok(0);
        }
        let pending = store::social_publications::pending_links(&self.surreal, "youtube", 100)
            .await
            .map_err(HandlerError::from)?;
        let mut recorded = 0;
        for publication in pending {
            let Some(receipt) = publication.provider_post_id.as_deref() else {
                continue;
            };
            let Some(post) = store::posts::find_by_id(&self.surreal, publication.post_id)
                .await
                .map_err(HandlerError::from)?
            else {
                continue;
            };
            let accounts = store::social_accounts::selected_for_brand(
                &self.surreal,
                &post.brand_key,
                &post.selected_social_accounts,
            )
            .await
            .map_err(HandlerError::from)?;
            let Some(account) = accounts
                .into_iter()
                .find(|account| account.provider == SocialProvider::Youtube)
            else {
                continue;
            };
            let target = Destination {
                provider: SocialProvider::Youtube,
                account_id: account.provider_account_id,
            };
            if self
                .read_permalink(publication.post_id, &post.brand_key, &target, receipt)
                .await?
            {
                recorded += 1;
            }
        }
        Ok(recorded)
    }

    async fn read_youtube_permalink(
        &self,
        post_id: Uuid,
        destination: &Destination,
        receipt: &str,
        credential: &SendCredential,
    ) -> Result<bool, HandlerError> {
        let response = self
            .http
            .get(format!("{}/videos", self.endpoints.youtube_api))
            .query(&[
                ("id", receipt),
                ("part", "snippet,status,processingDetails"),
            ])
            .bearer_auth(credential.access_token())
            .send()
            .await;
        let Ok(response) = response else {
            return Ok(false);
        };
        if !response.status().is_success() {
            return Ok(false);
        }
        let Ok(videos) = response.json::<YoutubeVideos>().await else {
            return Ok(false);
        };
        if !videos.items.iter().any(|video| {
            video.id == receipt
                && video.snippet.channel_id == destination.account_id
                && video.status.privacy_status == "public"
                && video.status.upload_status == "processed"
                && video.processing_details.processing_status == "succeeded"
        }) {
            return Ok(false);
        }
        Ok(store::social_publications::record_permalink(
            &self.surreal,
            post_id,
            "youtube",
            &format!("https://www.youtube.com/watch?v={receipt}"),
        )
        .await
        .is_ok())
    }

    pub(crate) async fn read_permalink(
        &self,
        post_id: Uuid,
        brand_key: &str,
        destination: &Destination,
        receipt: &str,
    ) -> Result<bool, HandlerError> {
        let provider = destination.provider;
        if self.egress.check().is_err() {
            return Ok(false);
        }
        let Ok(credential) = send_credential(
            &self.surreal,
            self.kms.as_ref(),
            &self.http,
            &self.refresh,
            brand_key,
            provider,
            &destination.account_id,
        )
        .await
        else {
            return Ok(false);
        };
        if provider == SocialProvider::Tiktok {
            return self
                .read_tiktok_permalink(post_id, destination, receipt, &credential)
                .await;
        }
        if provider == SocialProvider::Youtube {
            return self
                .read_youtube_permalink(post_id, destination, receipt, &credential)
                .await;
        }
        if provider == SocialProvider::X {
            if !receipt.bytes().all(|b| b.is_ascii_digit()) || receipt.is_empty() {
                return Ok(false);
            }
            let response = self
                .http
                .get(format!("{}/tweets/{receipt}", self.endpoints.x_api))
                .query(&[("tweet.fields", "author_id")])
                .bearer_auth(credential.access_token())
                .send()
                .await;
            let ProviderReply::Accepted(body) = read_reply(response).await else {
                return Ok(false);
            };
            if body.pointer("/data/id").and_then(serde_json::Value::as_str) != Some(receipt)
                || body
                    .pointer("/data/author_id")
                    .and_then(serde_json::Value::as_str)
                    != Some(destination.account_id.as_str())
            {
                return Ok(false);
            }
            return Ok(store::social_publications::record_permalink(
                &self.surreal,
                post_id,
                "x",
                &format!("https://x.com/i/web/status/{receipt}"),
            )
            .await
            .is_ok());
        }
        let (base, field) = match provider {
            SocialProvider::Facebook => (&self.endpoints.facebook_graph, "permalink_url"),
            SocialProvider::Instagram => (&self.endpoints.facebook_graph, "permalink"),
            SocialProvider::Threads => (&self.endpoints.threads, "permalink"),
            SocialProvider::Youtube
            | SocialProvider::Linkedin
            | SocialProvider::X
            | SocialProvider::Tiktok => return Ok(false),
        };
        let response = self
            .http
            .get(format!("{base}/{receipt}"))
            .query(&[
                ("fields", field),
                ("access_token", credential.access_token()),
            ])
            .send()
            .await;
        let ProviderReply::Accepted(body) = read_reply(response).await else {
            return Ok(false);
        };
        let Some(permalink) = provider_permalink(provider, &body) else {
            return Ok(false);
        };
        Ok(store::social_publications::record_permalink(
            &self.surreal,
            post_id,
            provider.as_str(),
            &permalink,
        )
        .await
        .is_ok())
    }

    /// Settle a claimed write from the provider's reply.
    async fn settle(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        reply: ProviderReply,
    ) -> Result<PublishOutcome, HandlerError> {
        match reply {
            ProviderReply::Accepted(body) => {
                let Some(id) = body.get("id").and_then(serde_json::Value::as_str) else {
                    return self
                        .unknown(
                            post_id,
                            provider,
                            "The provider accepted the post without an identifier",
                        )
                        .await;
                };
                let permalink = provider_permalink(provider, &body);
                store::social_publications::mark_confirmed(
                    &self.surreal,
                    post_id,
                    provider.as_str(),
                    id,
                    permalink.as_deref(),
                )
                .await
                .map_err(HandlerError::from)?;
                let mut outcome = PublishOutcome::new(provider, CONFIRMED);
                if permalink.is_none() {
                    outcome.link_pending = Some(id.to_string());
                }
                Ok(outcome)
            }
            ProviderReply::Refused(refusal) => self.refused(post_id, provider, &refusal).await,
            ProviderReply::Ambiguous => {
                self.unknown(
                    post_id,
                    provider,
                    "The provider's outcome was ambiguous; reconcile before retrying",
                )
                .await
            }
        }
    }
}

/// The public link a provider returned in `body`, if any.
///
/// Facebook names it `permalink_url` and may return a video's as a path on
/// `www.facebook.com`; Instagram and Threads name it `permalink`. A provider
/// receipt alone is never turned into a public link. `YouTube` and X links
/// require a separate readback of the receipt and its account identity.
fn provider_permalink(provider: SocialProvider, body: &serde_json::Value) -> Option<String> {
    let text = |field: &str| {
        body.get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let link = match provider {
        SocialProvider::Facebook => text("permalink_url").map(|link| {
            if link.starts_with('/') {
                format!("https://www.facebook.com{link}")
            } else {
                link.to_string()
            }
        }),
        SocialProvider::Instagram | SocialProvider::Threads => {
            text("permalink").map(str::to_string)
        }
        SocialProvider::Linkedin
        | SocialProvider::X
        | SocialProvider::Tiktok
        | SocialProvider::Youtube => None,
    }?;
    link.starts_with("https://").then_some(link)
}

const fn provider_label(provider: SocialProvider) -> &'static str {
    match provider {
        SocialProvider::Facebook => "Facebook",
        SocialProvider::Instagram => "Instagram",
        SocialProvider::Threads => "Threads",
        SocialProvider::Youtube => "YouTube",
        SocialProvider::Linkedin => "LinkedIn",
        SocialProvider::X => "X",
        SocialProvider::Tiktok => "TikTok",
    }
}

fn bounded(value: &str) -> String {
    value.chars().take(300).collect()
}

async fn tiktok_json(
    reply: Result<reqwest::Response, reqwest::Error>,
) -> Option<serde_json::Value> {
    let reply = reply.ok()?;
    if !reply.status().is_success() {
        return None;
    }
    let bytes = reply.bytes().await.ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let body: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if body.get("code").and_then(serde_json::Value::as_i64) != Some(0) {
        return None;
    }
    body.get("data").cloned()
}

/// A provider's answer to one request.
enum ProviderReply {
    /// A 2xx JSON body.
    Accepted(serde_json::Value),
    /// The provider answered with an error and did not act on the request.
    Refused(ProviderRefusal),
    /// No answer, an unreadable one, or a server error: the request may or
    /// may not have taken effect.
    Ambiguous,
}

struct ProviderRefusal {
    kind: RefusalKind,
    message: String,
}

/// Why a provider refused a request it did not act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefusalKind {
    /// The token is invalid, expired, or revoked.
    Authorization,
    /// The account or app exceeded a provider rate limit.
    RateLimited,
    /// The provider marked the error temporary.
    Temporary,
    /// A terminal refusal: retrying the same request fails the same way.
    Rejected,
}

impl ProviderRefusal {
    fn rejected(message: &str) -> Self {
        Self {
            kind: RefusalKind::Rejected,
            message: message.into(),
        }
    }

    fn reason(&self, provider: SocialProvider) -> String {
        let reason = match self.kind {
            RefusalKind::Authorization => {
                return format!(
                    "Reconnect the {} account: the provider rejected its authorization",
                    provider.as_str()
                );
            }
            RefusalKind::RateLimited => {
                "Rate limited by the provider; nothing was published. Retry later"
            }
            RefusalKind::Temporary => {
                "The provider reported a temporary failure; nothing was published. Retry later"
            }
            RefusalKind::Rejected => "The provider rejected the post",
        };
        bounded(&format!("{reason}: {}", self.message))
    }
}

/// Classify a refusal from its HTTP status and error body.
///
/// Meta reports an invalid, expired, or revoked token as code 190 (or 102),
/// throttling as codes 4, 17, 32, 613 and the 80001–80014 business-use-case
/// range, and a temporary error with `is_transient`. Google answers 401 for a
/// bad token and names quota errors in `error.errors[].reason`.
fn refusal_kind(status: reqwest::StatusCode, error: Option<&serde_json::Value>) -> RefusalKind {
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_i64);
    let google_reason = |reasons: &[&str]| {
        error
            .and_then(|error| error.get("errors"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|errors| {
                errors.iter().any(|item| {
                    item.get("reason")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|reason| reasons.contains(&reason))
                })
            })
    };
    if status == reqwest::StatusCode::UNAUTHORIZED || matches!(code, Some(190 | 102)) {
        RefusalKind::Authorization
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || matches!(code, Some(4 | 17 | 32 | 613 | 80_001..=80_014))
        || google_reason(&[
            "rateLimitExceeded",
            "userRateLimitExceeded",
            "quotaExceeded",
        ])
    {
        RefusalKind::RateLimited
    } else if error
        .and_then(|error| error.get("is_transient"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        || matches!(code, Some(1 | 2))
    {
        RefusalKind::Temporary
    } else {
        RefusalKind::Rejected
    }
}

/// Classify a provider response.
async fn read_reply(response: Result<reqwest::Response, reqwest::Error>) -> ProviderReply {
    let Ok(response) = response else {
        return ProviderReply::Ambiguous;
    };
    let status = response.status();
    let body = match response.bytes().await {
        Ok(body) if body.len() <= 64 * 1024 => body,
        _ => return ProviderReply::Ambiguous,
    };
    let json = serde_json::from_slice::<serde_json::Value>(&body).ok();
    if status.is_success() {
        return json.map_or(ProviderReply::Ambiguous, ProviderReply::Accepted);
    }
    if !status.is_client_error() {
        return ProviderReply::Ambiguous;
    }
    let error = json.as_ref().and_then(|value| value.get("error"));
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| format!("HTTP {status}"), str::to_string);
    ProviderReply::Refused(ProviderRefusal {
        kind: refusal_kind(status, error),
        message,
    })
}

/// A conservative bound: every scalar weighs two and every URL at least 23.
/// This admits ordinary and CJK text safely without assuming Premium limits.
fn x_text_eligible(text: &str) -> bool {
    // Keep raw punctuation weight and reserve shortening weight for every possible URL.
    // Domain separators and protocol prefixes deliberately overcount uncertain candidates.
    let candidates = text
        .chars()
        .filter(|ch| matches!(ch, '.' | '\u{3002}' | '\u{ff0e}' | '\u{ff61}'))
        .count()
        .saturating_add(text.matches("http://").count())
        .saturating_add(text.matches("https://").count());
    !text.trim().is_empty()
        && text
            .chars()
            .count()
            .saturating_mul(2)
            .saturating_add(candidates.saturating_mul(23))
            <= 280
}

fn caption(post: &PostSnapshot, provider: SocialProvider) -> String {
    let text = match provider {
        SocialProvider::Facebook => post.facebook_caption.as_deref(),
        SocialProvider::Instagram => post.instagram_caption.as_deref(),
        SocialProvider::Threads => post.threads_caption.as_deref(),
        SocialProvider::Youtube => post.youtube_caption.as_deref(),
        SocialProvider::Linkedin | SocialProvider::X | SocialProvider::Tiktok => None,
    }
    .unwrap_or(&post.description);
    tag_brand_links(text, provider)
}

/// The query keys Plausible reads as a visit's source. A link already
/// carrying one keeps the author's choice.
const SOURCE_KEYS: &[&str] = &["ref", "utm_source", "source"];

/// Add `?ref=<provider>` to every `http(s)` link in `text` that points at one
/// of the firm's own brand hosts, so Plausible attributes the visit (and any
/// conversion in it) to the platform even when the app strips the referrer.
/// Links elsewhere, and links that already name a source, are left as written.
fn tag_brand_links(text: &str, provider: SocialProvider) -> String {
    let mut tagged = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest
        .find("https://")
        .into_iter()
        .chain(rest.find("http://"))
        .min()
    {
        tagged.push_str(&rest[..start]);
        let token_len = rest[start..]
            .find(char::is_whitespace)
            .unwrap_or(rest.len() - start);
        let token = &rest[start..start + token_len];
        let link = token.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']);
        tagged.push_str(&tag_link(link, provider).unwrap_or_else(|| link.to_string()));
        tagged.push_str(&token[link.len()..]);
        rest = &rest[start + token_len..];
    }
    tagged.push_str(rest);
    tagged
}

fn tag_link(link: &str, provider: SocialProvider) -> Option<String> {
    let mut url = url::Url::parse(link).ok()?;
    let host = url.host_str()?;
    let ours = views::brand::BrandKey::ALL
        .iter()
        .any(|key| key.apex() == host || key.hosts().contains(&host));
    if !ours
        || url
            .query_pairs()
            .any(|(key, _)| SOURCE_KEYS.contains(&&*key))
    {
        return None;
    }
    url.query_pairs_mut().append_pair("ref", provider.as_str());
    Some(url.into())
}

#[derive(Deserialize)]
struct YoutubeVideos {
    items: Vec<YoutubeVideo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct YoutubeVideo {
    id: String,
    snippet: YoutubeSnippet,
    status: YoutubeStatus,
    processing_details: YoutubeProcessing,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct YoutubeSnippet {
    channel_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct YoutubeStatus {
    privacy_status: String,
    upload_status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct YoutubeProcessing {
    processing_status: String,
}

#[derive(Clone)]
struct ProviderEndpoints {
    facebook_graph: String,
    threads: String,
    youtube_upload: String,
    youtube_api: String,
    x_api: String,
    tiktok: String,
    linkedin_rest: String,
}

impl Default for ProviderEndpoints {
    fn default() -> Self {
        Self {
            facebook_graph: "https://graph.facebook.com/v26.0".into(),
            threads: "https://graph.threads.net/v1.0".into(),
            youtube_upload: "https://www.googleapis.com/upload/youtube/v3".into(),
            youtube_api: "https://www.googleapis.com/youtube/v3".into(),
            x_api: "https://api.x.com/2".into(),
            tiktok: "https://business-api.tiktok.com/open_api/v1.3".into(),
            linkedin_rest: "https://api.linkedin.com/rest".into(),
        }
    }
}

pub(crate) fn publication_outcome(outcomes: &[PublishOutcome]) -> &'static str {
    if !outcomes.is_empty() && outcomes.iter().all(|outcome| outcome.state == BLOCKED) {
        "social_publishing_disabled"
    } else if outcomes.iter().any(|outcome| outcome.state == FAILED) {
        "partial_failure"
    } else if outcomes.iter().any(|outcome| outcome.state == UNKNOWN) {
        "reconciliation_required"
    } else {
        "published"
    }
}

#[cfg(test)]
mod tests {
    use super::{CONFIRMED, FAILED, UNKNOWN};
    use std::time::Duration;

    use super::{
        container_policy, publication_outcome, ContainerStatus, Destination, PostSnapshot,
        ProviderEndpoints, PublishOutcome, Publisher, Step,
    };
    use chrono::Utc;
    use cloud::StorageService as _;
    use store::brands::NewBrand;
    use store::config::{SocialEgressDenied, SocialEgressGate};
    use store::firms::NewFirm;
    use store::persons::{NewPerson, Role};
    use store::social_accounts::{ConnectSocialAccount, SocialProvider};
    use store::social_publications::Admission;
    use uuid::Uuid;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};
    use workflows::social_publish::TokenRefreshConfig;

    const VIDEO: &str = "synthetic/video.mp4";

    fn post(video_asset: bool) -> PostSnapshot {
        PostSnapshot {
            brand_key: "synthetic-brand".into(),
            title: "Synthetic update".into(),
            description: "Synthetic description".into(),
            video_asset: video_asset.then(|| VIDEO.into()),
            vertical_video: video_asset,
            tiktok_ai_generated: None,
            publish_at: Some(Utc::now()),
            status: "scheduled".into(),
            facebook_caption: Some("Facebook copy".into()),
            instagram_caption: Some("Instagram copy".into()),
            threads_caption: Some("Threads copy".into()),
            youtube_caption: Some("YouTube copy".into()),
            destinations: Vec::new(),
        }
    }

    async fn tiktok_fixture(fixture: &Fixture, snapshot: &PostSnapshot) -> Uuid {
        let id = fixture.blog_post().await;
        fixture.db.query("UPDATE $id SET publish_at=<datetime>$publish_at,video_asset=$video,vertical_video=true,tiktok_ai_generated=false,description=$description")
            .bind(("id",store::surreal::record_id("post",id)))
            .bind(("publish_at",snapshot.publish_at.unwrap().to_rfc3339()))
            .bind(("description",snapshot.description.clone()))
            .bind(("video",VIDEO.to_string())).await.unwrap().check().unwrap();
        fixture
            .connect(
                SocialProvider::Tiktok,
                "tiktok-user",
                SocialProvider::Tiktok.required_scopes(),
                None,
            )
            .await;
        store::posts::record_video(&fixture.db, "synthetic-brand", VIDEO, 1080, 1920)
            .await
            .unwrap();
        fixture
            .storage
            .put(
                VIDEO,
                &crate::tiktok_business::tests::video(30, 30),
                "video/mp4",
            )
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn tiktok_missing_disclosure_blocks_without_provider_requests() {
        let fixture = Fixture::new("TikTok disclosure").await;
        let server = MockServer::start().await;
        let id = fixture.blog_post().await;
        let publisher = fixture.publisher(&server, allowed());
        let target = Destination {
            provider: SocialProvider::Tiktok,
            account_id: "tiktok-user".into(),
        };
        let result = publisher.prepare(id, &post(true), &target).await.unwrap();
        assert!(matches!(result,Step::Settled(PublishOutcome{state,..}) if state==FAILED));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn tiktok_accepted_task_reconciles_without_duplicate_send() {
        let fixture = Fixture::new("TikTok async").await;
        let server = MockServer::start().await;
        let mut snapshot = post(true);
        snapshot.tiktok_ai_generated = Some(false);
        let id = tiktok_fixture(&fixture, &snapshot).await;
        let publisher = fixture.publisher(&server, allowed());
        let target = Destination {
            provider: SocialProvider::Tiktok,
            account_id: "tiktok-user".into(),
        };
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/tiktok/business/property/list/"))
            .and(matchers::query_param("app_id", "synthetic-app"))
            .and(matchers::query_param("secret", "synthetic-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"url_property_info_list":[{"property_type":1,"property_status":1,"url":"example.test"}]}})))
            .mount(&server).await;
        Mock::given(matchers::method("GET")).and(matchers::path("/tiktok/business/video/settings/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"privacy_level_options":["PUBLIC_TO_EVERYONE"],"max_video_post_duration_sec":600}}))).mount(&server).await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/tiktok/business/video/publish/"))
            .and(matchers::header("Access-Token", "synthetic-access-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"code":0,"data":{"share_id":"synthetic-task"}}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            publisher.prepare(id, &snapshot, &target).await.unwrap(),
            Step::Continue(())
        ));
        assert!(
            matches!(publisher.start_tiktok(id,&snapshot,&target).await.unwrap(),Step::Continue(task) if task=="synthetic-task")
        );
        assert!(
            matches!(publisher.start_tiktok(id,&snapshot,&target).await.unwrap(),Step::Continue(task) if task=="synthetic-task")
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "POST")
                .count(),
            1
        );
        let publications = store::social_publications::for_posts(&fixture.db, &[id])
            .await
            .unwrap();
        assert_eq!(publications[0].state, UNKNOWN);
        assert!(publications[0].provider_post_id.is_none());
        Mock::given(matchers::method("GET")).and(matchers::path("/tiktok/business/publish/status/"))
            .and(matchers::query_param("publish_id","synthetic-task"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"status":"PUBLISH_COMPLETE","post_ids":["synthetic-public"]}}))).mount(&server).await;
        assert!(
            matches!(publisher.poll_tiktok(id,&snapshot,&target,"synthetic-task").await.unwrap(),Step::Settled(outcome) if outcome.state==CONFIRMED)
        );
        let publications = store::social_publications::for_posts(&fixture.db, &[id])
            .await
            .unwrap();
        assert_eq!(
            publications[0].provider_post_id.as_deref(),
            Some("synthetic-public")
        );
        assert!(publications[0].permalink.is_none());
        assert!(
            matches!(publisher.poll_tiktok(id, &snapshot, &target, "synthetic-task").await.unwrap(),
            Step::Settled(outcome) if outcome.state == CONFIRMED && outcome.link_pending.as_deref() == Some("synthetic-public"))
        );
        Mock::given(matchers::method("GET")).and(matchers::path("/tiktok/business/video/list/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"videos":[{"item_id":"synthetic-public","share_url":"https://www.tiktok.com/@synthetic/video/123","media_type":"VIDEO","is_ad":false}]}}))).mount(&server).await;
        assert!(publisher
            .read_permalink(id, &snapshot.brand_key, &target, "synthetic-public")
            .await
            .unwrap());
        assert!(
            matches!(publisher.prepare(id,&snapshot,&target).await.unwrap(),Step::Settled(outcome) if outcome.state==CONFIRMED)
        );
    }

    #[tokio::test]
    async fn tiktok_unverified_media_property_refuses_before_publish() {
        let fixture = Fixture::new("TikTok unverified media").await;
        let server = MockServer::start().await;
        let mut snapshot = post(true);
        snapshot.tiktok_ai_generated = Some(false);
        let id = tiktok_fixture(&fixture, &snapshot).await;
        let publisher = fixture.publisher(&server, allowed());
        let target = Destination {
            provider: SocialProvider::Tiktok,
            account_id: "tiktok-user".into(),
        };
        Mock::given(matchers::method("GET")).and(matchers::path("/tiktok/business/video/settings/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"privacy_level_options":["PUBLIC_TO_EVERYONE"],"max_video_post_duration_sec":600}}))).mount(&server).await;
        Mock::given(matchers::method("GET")).and(matchers::path("/tiktok/business/property/list/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"url_property_info_list":[{"property_type":1,"property_status":0,"url":"example.test"}]}}))).mount(&server).await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        publisher.prepare(id, &snapshot, &target).await.unwrap();
        assert!(
            matches!(publisher.start_tiktok(id, &snapshot, &target).await.unwrap(),Step::Settled(outcome) if outcome.state==FAILED)
        );
    }

    #[tokio::test]
    async fn tiktok_final_write_refuses_a_reconnected_credential() {
        let fixture = Fixture::new("TikTok credential race").await;
        let server = silent_provider().await;
        let mut snapshot = post(true);
        snapshot.tiktok_ai_generated = Some(false);
        let id = tiktok_fixture(&fixture, &snapshot).await;
        let publisher = fixture.publisher(&server, allowed());
        let target = Destination {
            provider: SocialProvider::Tiktok,
            account_id: "tiktok-user".into(),
        };
        publisher.prepare(id, &snapshot, &target).await.unwrap();
        let original = publisher
            .writable(id, &snapshot, &target)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            publisher
                .tiktok_final_credential(id, &snapshot, &target, &original)
                .await
                .unwrap(),
            Step::Continue(_)
        ));
        store::social_accounts::connect(
            &fixture.db,
            ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: fixture.admin,
                brand_key: "synthetic-brand",
                provider: SocialProvider::Tiktok,
                provider_account_id: "tiktok-user",
                display_name: None,
                scopes: &SocialProvider::Tiktok
                    .required_scopes()
                    .iter()
                    .map(|scope| (*scope).to_string())
                    .collect::<Vec<_>>(),
                expires_at: None,
                access_token: "synthetic-reconnected-token",
                refresh_token: None,
            },
            &fixture.kms,
        )
        .await
        .unwrap();
        assert!(
            matches!(publisher.tiktok_final_credential(id, &snapshot, &target, &original).await.unwrap(),
            Step::Settled(outcome) if outcome.state == FAILED)
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn tiktok_failed_poll_replay_returns_terminal_without_provider_request() {
        let fixture = Fixture::new("TikTok failed replay").await;
        let server = silent_provider().await;
        let snapshot = post(true);
        let id = fixture.blog_post().await;
        store::social_publications::admit(&fixture.db, id, "tiktok")
            .await
            .unwrap();
        store::social_publications::begin_provider_write(&fixture.db, id, "tiktok")
            .await
            .unwrap();
        store::social_publications::record_provider_task(
            &fixture.db,
            id,
            "tiktok",
            "synthetic-task",
        )
        .await
        .unwrap();
        store::social_publications::mark_failed(&fixture.db, id, "tiktok", "synthetic failure")
            .await
            .unwrap();
        let publisher = fixture.publisher(&server, allowed());
        let target = Destination {
            provider: SocialProvider::Tiktok,
            account_id: "tiktok-user".into(),
        };
        for _ in 0..2 {
            assert!(
                matches!(publisher.poll_tiktok(id, &snapshot, &target, "synthetic-task").await.unwrap(),
                Step::Settled(outcome) if outcome.state == FAILED)
            );
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    fn allowed() -> SocialEgressGate {
        SocialEgressGate::new(|| Ok(()))
    }

    fn denied_by(
        environment: Option<&'static str>,
        simulated: Option<&'static str>,
    ) -> SocialEgressGate {
        SocialEgressGate::new(move || {
            store::config::social_egress_from(|key| match key {
                store::config::NAVIGATOR_ENVIRONMENT => environment.map(str::to_owned),
                store::config::NAVIGATOR_SIMULATED_MATTERS => simulated.map(str::to_owned),
                _ => None,
            })
        })
    }

    /// A provider double that fails the test if any request reaches it.
    async fn silent_provider() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(matchers::any())
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        server
    }

    struct Fixture {
        db: store::surreal::SurrealDb,
        kms: cloud::FakeKms,
        storage: std::sync::Arc<cloud::FsStorage>,
        admin: Uuid,
        _dir: tempfile::TempDir,
    }

    impl Fixture {
        async fn new(name: &str) -> Self {
            let db = store::test_support::mem_surreal().await;
            let admin = store::persons::create(
                &db,
                &NewPerson::with_role(
                    name,
                    format!("{name}-{}@example.com", Uuid::now_v7()),
                    Role::Admin,
                ),
            )
            .await
            .unwrap();
            let firm = store::firms::create(
                &db,
                &NewFirm {
                    name: format!("{name} Firm"),
                    status: "active".into(),
                    entity_id: store::test_support::seed_entity(&db).await,
                    admin_dri_person_id: admin.id,
                },
            )
            .await
            .unwrap();
            store::brands::create(
                &db,
                Role::Admin,
                Some(admin.id),
                &NewBrand {
                    name: format!("{name} Brand"),
                    key: "synthetic-brand".into(),
                    firm_id: Some(firm.id),
                    primary_color: None,
                    accent_color: None,
                    typeface: None,
                },
            )
            .await
            .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
            storage
                .put(VIDEO, b"synthetic video", "video/mp4")
                .await
                .unwrap();
            Self {
                db,
                kms: cloud::FakeKms::new(name),
                storage: std::sync::Arc::new(storage),
                admin: admin.id,
                _dir: dir,
            }
        }

        async fn connect(
            &self,
            provider: SocialProvider,
            account_id: &str,
            scopes: &[&str],
            expires_at: Option<chrono::DateTime<Utc>>,
        ) {
            let scopes = scopes
                .iter()
                .map(|scope| (*scope).to_string())
                .collect::<Vec<_>>();
            store::social_accounts::connect(
                &self.db,
                ConnectSocialAccount {
                    actor_role: Role::Admin,
                    actor_person_id: self.admin,
                    brand_key: "synthetic-brand",
                    provider,
                    provider_account_id: account_id,
                    display_name: None,
                    scopes: &scopes,
                    expires_at,
                    access_token: "synthetic-access-token",
                    refresh_token: None,
                },
                &self.kms,
            )
            .await
            .unwrap();
        }

        async fn connect_all(&self) {
            for (provider, account) in [
                (SocialProvider::Facebook, "facebook-page"),
                (SocialProvider::Instagram, "instagram-user"),
                (SocialProvider::Threads, "threads-user"),
                (SocialProvider::Youtube, "youtube-channel"),
                (SocialProvider::Linkedin, "1001"),
                (SocialProvider::X, "1002"),
            ] {
                self.connect(provider, account, provider.required_scopes(), None)
                    .await;
            }
        }

        fn publisher(&self, server: &MockServer, egress: SocialEgressGate) -> Publisher {
            Publisher {
                surreal: self.db.clone(),
                kms: std::sync::Arc::new(self.kms.clone()),
                storage: self.storage.clone(),
                egress,
                refresh: TokenRefreshConfig {
                    x_token_url: format!("{}/token", server.uri()),
                    tiktok_token_url: format!("{}/tiktok-token", server.uri()),
                    tiktok_client: Some(("synthetic-app".into(), "synthetic-secret".into())),
                    x_client: Some(("x-id".into(), "x-secret".into())),
                    threads_refresh_url: format!("{}/refresh_access_token", server.uri()),
                    google_token_url: format!("{}/token", server.uri()),
                    youtube_client: None,
                },
                endpoints: ProviderEndpoints {
                    facebook_graph: server.uri(),
                    threads: server.uri(),
                    youtube_upload: server.uri(),
                    youtube_api: server.uri(),
                    x_api: server.uri(),
                    tiktok: format!("{}/tiktok", server.uri()),
                    linkedin_rest: server.uri(),
                },
                http: reqwest::Client::builder()
                    .timeout(Duration::from_millis(500))
                    .build()
                    .unwrap(),
                video_url: |key| format!("https://assets.example.test/{key}"),
            }
        }

        async fn blog_post(&self) -> Uuid {
            let id = Uuid::now_v7();
            self.db
                .query("CREATE $id SET brand_key = 'synthetic-brand', slug = $slug, title = 'Synthetic', description = '', body_html = '', status = 'scheduled', inserted_at = time::now(), updated_at = time::now()")
                .bind(("id", store::surreal::record_id("post", id)))
                .bind(("slug", id.to_string()))
                .await
                .unwrap()
                .check()
                .unwrap();
            id
        }

        async fn publication(
            &self,
            post_id: Uuid,
        ) -> store::social_publications::SocialPublication {
            store::social_publications::for_posts(&self.db, &[post_id])
                .await
                .unwrap()
                .remove(0)
        }
    }

    fn destination(provider: SocialProvider) -> Destination {
        Destination {
            provider,
            account_id: match provider {
                SocialProvider::Facebook => "facebook-page",
                SocialProvider::Instagram => "instagram-user",
                SocialProvider::Threads => "threads-user",
                SocialProvider::Youtube => "youtube-channel",
                SocialProvider::Linkedin => "1001",
                SocialProvider::X => "1002",
                SocialProvider::Tiktok => "tiktok-user",
            }
            .into(),
        }
    }

    /// The workflow handler's step order, without the Restate journal or its
    /// durable sleeps.
    async fn drive(
        publisher: &Publisher,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> PublishOutcome {
        let outcome = send(publisher, post_id, post, destination).await;
        if let Some(receipt) = outcome.link_pending.as_deref() {
            publisher
                .read_permalink(post_id, &post.brand_key, destination, receipt)
                .await
                .unwrap();
        }
        outcome
    }

    async fn send(
        publisher: &Publisher,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> PublishOutcome {
        if let Step::Settled(outcome) = publisher.prepare(post_id, post, destination).await.unwrap()
        {
            return outcome;
        }
        let Some(policy) = container_policy(destination.provider) else {
            return publisher
                .send_direct(post_id, post, destination)
                .await
                .unwrap();
        };
        let container = match publisher
            .create_container(post_id, post, destination)
            .await
            .unwrap()
        {
            Step::Continue(container) => container,
            Step::Settled(outcome) => return outcome,
        };
        for _ in 0..policy.checks {
            match publisher
                .container_status(post_id, post, destination, &container)
                .await
                .unwrap()
            {
                Step::Settled(outcome) => return outcome,
                Step::Continue(ContainerStatus::Processing) => {}
                Step::Continue(ContainerStatus::Ready) => {
                    return publisher
                        .publish_container(post_id, post, destination, &container)
                        .await
                        .unwrap();
                }
            }
        }
        publisher
            .fail(post_id, destination.provider, "still processing")
            .await
            .unwrap()
    }

    fn json(body: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(body)
    }

    async fn mount_container(server: &MockServer, created: u64) {
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/instagram-user/media"))
            .and(matchers::body_string_contains("media_type=REELS"))
            .and(matchers::body_string_contains(
                "video_url=https%3A%2F%2Fassets.example.test%2Fsynthetic%2Fvideo.mp4",
            ))
            .respond_with(json(serde_json::json!({"id": "container-1"})))
            .expect(created)
            .mount(server)
            .await;
    }

    async fn mount_status(server: &MockServer, status: &str, times: u64) {
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/container-1"))
            .and(matchers::query_param("fields", "status_code"))
            .respond_with(json(
                serde_json::json!({"status_code": status, "id": "container-1"}),
            ))
            .up_to_n_times(times)
            .mount(server)
            .await;
    }

    async fn mount_publish(server: &MockServer, response: ResponseTemplate, expected: u64) {
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/instagram-user/media_publish"))
            .and(matchers::body_string_contains("creation_id=container-1"))
            .respond_with(response)
            .expect(expected)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn facebook_success_persists_the_provider_confirmation() {
        let fixture = Fixture::new("facebook-confirm").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/facebook-page/feed"))
            .and(matchers::body_string_contains("message=Facebook+copy"))
            .respond_with(json(serde_json::json!({
                "id": "provider-post-1",
                "permalink_url": "https://www.facebook.com/provider-post-1"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let post_id = fixture.blog_post().await;
        let outcome = drive(
            &fixture.publisher(&server, allowed()),
            post_id,
            &post(false),
            &destination(SocialProvider::Facebook),
        )
        .await;
        assert_eq!(outcome.state, "confirmed");
        let row = fixture.publication(post_id).await;
        assert_eq!(row.provider_post_id.as_deref(), Some("provider-post-1"));
        assert_eq!(row.last_error, None);
    }

    #[tokio::test]
    async fn instagram_waits_for_a_ready_container_and_publishes_it_once() {
        let fixture = Fixture::new("instagram-ready").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        mount_container(&server, 1).await;
        mount_status(&server, "IN_PROGRESS", 2).await;
        mount_status(&server, "FINISHED", 1).await;
        mount_publish(&server, json(serde_json::json!({"id": "ig-media-1"})), 1).await;
        let post_id = fixture.blog_post().await;
        let outcome = drive(
            &fixture.publisher(&server, allowed()),
            post_id,
            &post(true),
            &destination(SocialProvider::Instagram),
        )
        .await;
        assert_eq!(outcome.state, "confirmed");
        assert_eq!(
            fixture
                .publication(post_id)
                .await
                .provider_post_id
                .as_deref(),
            Some("ig-media-1")
        );
    }

    #[tokio::test]
    async fn a_failed_or_never_ready_container_is_never_published() {
        let fixture = Fixture::new("instagram-error").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        mount_container(&server, 2).await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/container-1"))
            .respond_with(json(serde_json::json!({"status_code": "ERROR"})))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount_status(&server, "IN_PROGRESS", 100).await;
        mount_publish(&server, json(serde_json::json!({"id": "never"})), 0).await;
        let publisher = fixture.publisher(&server, allowed());

        let failed_post = fixture.blog_post().await;
        let failed = drive(
            &publisher,
            failed_post,
            &post(true),
            &destination(SocialProvider::Instagram),
        )
        .await;
        assert_eq!(failed.state, "failed");
        assert!(fixture
            .publication(failed_post)
            .await
            .last_error
            .unwrap()
            .starts_with("The media container failed"));

        let pending_post = fixture.blog_post().await;
        let pending = drive(
            &publisher,
            pending_post,
            &post(true),
            &destination(SocialProvider::Instagram),
        )
        .await;
        assert_eq!(pending.state, "failed");
        // A known failure: the next admission may send again.
        assert!(matches!(
            store::social_publications::admit(&fixture.db, pending_post, "instagram")
                .await
                .unwrap(),
            Admission::Admitted(_)
        ));
    }

    #[tokio::test]
    async fn a_restart_between_create_and_publish_neither_recreates_nor_resends() {
        let fixture = Fixture::new("instagram-restart").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        mount_container(&server, 1).await;
        mount_publish(
            &server,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id": "ig-media-1"}))
                .set_delay(Duration::from_secs(2)),
            1,
        )
        .await;
        let publisher = fixture.publisher(&server, allowed());
        let post_id = fixture.blog_post().await;
        let target = destination(SocialProvider::Instagram);
        assert!(matches!(
            publisher
                .prepare(post_id, &post(true), &target)
                .await
                .unwrap(),
            Step::Continue(())
        ));
        // Journaled: a resumed invocation replays this id instead of creating.
        let Step::Continue(container) = publisher
            .create_container(post_id, &post(true), &target)
            .await
            .unwrap()
        else {
            panic!("container was not created");
        };
        // The publish step is interrupted after its request left.
        let interrupted = publisher
            .publish_container(post_id, &post(true), &target, &container)
            .await
            .unwrap();
        assert_eq!(interrupted.state, "unknown");
        // Replayed after the restart, the step finds its write claimed.
        let replayed = publisher
            .publish_container(post_id, &post(true), &target, &container)
            .await
            .unwrap();
        assert_eq!(replayed.state, "unknown");
        assert!(matches!(
            store::social_publications::admit(&fixture.db, post_id, "instagram")
                .await
                .unwrap(),
            Admission::ReconciliationRequired(_)
        ));
    }

    #[tokio::test]
    async fn a_revoked_provider_token_asks_for_a_reconnect() {
        let fixture = Fixture::new("instagram-revoked").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        mount_container(&server, 1).await;
        mount_status(&server, "FINISHED", 1).await;
        mount_publish(
            &server,
            ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {"message": "Error validating access token", "type": "OAuthException", "code": 190}
            })),
            1,
        )
        .await;
        let post_id = fixture.blog_post().await;
        let outcome = drive(
            &fixture.publisher(&server, allowed()),
            post_id,
            &post(true),
            &destination(SocialProvider::Instagram),
        )
        .await;
        assert_eq!(outcome.state, "failed");
        assert_eq!(
            fixture.publication(post_id).await.last_error.as_deref(),
            Some("Reconnect the instagram account: the provider rejected its authorization")
        );
    }

    #[tokio::test]
    async fn an_unknown_publish_result_requires_reconciliation() {
        let fixture = Fixture::new("threads-unknown").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/threads-user/threads"))
            .and(matchers::body_string_contains("media_type=TEXT"))
            .respond_with(json(serde_json::json!({"id": "container-1"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/container-1"))
            .and(matchers::query_param("fields", "status,error_message"))
            .respond_with(json(serde_json::json!({"status": "FINISHED"})))
            .mount(&server)
            .await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/threads-user/threads_publish"))
            .respond_with(ResponseTemplate::new(502))
            .expect(1)
            .mount(&server)
            .await;
        let post_id = fixture.blog_post().await;
        let publisher = fixture.publisher(&server, allowed());
        let outcome = drive(
            &publisher,
            post_id,
            &post(false),
            &destination(SocialProvider::Threads),
        )
        .await;
        assert_eq!(outcome.state, "unknown");
        // A second run reconciles instead of sending again.
        let rerun = drive(
            &publisher,
            post_id,
            &post(false),
            &destination(SocialProvider::Threads),
        )
        .await;
        assert_eq!(rerun.state, "unknown");
    }

    #[tokio::test]
    async fn a_text_only_post_reaches_eligible_destinations_without_instagram_requests() {
        let fixture = Fixture::new("partial").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/facebook-page/feed"))
            .respond_with(json(
                serde_json::json!({"id": "fb-1", "permalink_url": "https://www.facebook.com/fb-1"}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(matchers::path_regex("^/instagram-user/"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let publisher = fixture.publisher(&server, allowed());
        let post_id = fixture.blog_post().await;
        let facebook = drive(
            &publisher,
            post_id,
            &post(false),
            &destination(SocialProvider::Facebook),
        )
        .await;
        let instagram = drive(
            &publisher,
            post_id,
            &post(false),
            &destination(SocialProvider::Instagram),
        )
        .await;
        assert_eq!(facebook.state, "confirmed");
        assert_eq!(instagram.state, "failed");
        assert_eq!(
            publication_outcome(&[facebook, instagram]),
            "partial_failure"
        );
        let rows = store::social_publications::for_posts(&fixture.db, &[post_id])
            .await
            .unwrap();
        let instagram_row = rows.iter().find(|row| row.provider == "instagram").unwrap();
        assert_eq!(
            instagram_row.last_error.as_deref(),
            Some("Instagram publishes video only; this post has no video")
        );
    }

    #[tokio::test]
    async fn expired_tokens_and_missing_permissions_make_zero_provider_calls() {
        let fixture = Fixture::new("lifecycle").await;
        fixture
            .connect(
                SocialProvider::Threads,
                "threads-user",
                SocialProvider::Threads.required_scopes(),
                Some(Utc::now() - chrono::Duration::hours(1)),
            )
            .await;
        fixture
            .connect(
                SocialProvider::Instagram,
                "instagram-user",
                &[
                    "instagram_business_basic",
                    "instagram_business_content_publish",
                ],
                None,
            )
            .await;
        let server = silent_provider().await;
        let publisher = fixture.publisher(&server, allowed());

        let expired = fixture.blog_post().await;
        let outcome = drive(
            &publisher,
            expired,
            &post(false),
            &destination(SocialProvider::Threads),
        )
        .await;
        assert_eq!(outcome.state, "failed");
        assert_eq!(
            fixture.publication(expired).await.last_error.as_deref(),
            Some("Reconnect the threads account: its authorization expired")
        );

        let mismatched = fixture.blog_post().await;
        let outcome = drive(
            &publisher,
            mismatched,
            &post(true),
            &destination(SocialProvider::Instagram),
        )
        .await;
        assert_eq!(outcome.state, "failed");
        assert!(fixture
            .publication(mismatched)
            .await
            .last_error
            .unwrap()
            .starts_with("Reconnect the instagram account: it is missing"));
        server.verify().await;
    }

    async fn mount_youtube_upload(server: &MockServer) {
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/videos"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("location", format!("{}/upload/session", server.uri())),
            )
            .expect(1)
            .mount(server)
            .await;
        Mock::given(matchers::method("PUT"))
            .and(matchers::path("/upload/session"))
            .and(matchers::body_bytes(b"synthetic video".to_vec()))
            .respond_with(json(serde_json::json!({"id": "youtube-video-1"})))
            .expect(1)
            .mount(server)
            .await;
    }

    fn youtube_public_video() -> serde_json::Value {
        serde_json::json!({"items": [{
            "id": "youtube-video-1",
            "snippet": {"channelId": "youtube-channel"},
            "status": {"privacyStatus": "public", "uploadStatus": "processed"},
            "processingDetails": {"processingStatus": "succeeded"}
        }]})
    }

    async fn mount_youtube_read(
        server: &MockServer,
        body: serde_json::Value,
        status: u16,
        reads: u64,
    ) {
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/videos"))
            .and(matchers::query_param("id", "youtube-video-1"))
            .and(matchers::query_param(
                "part",
                "snippet,status,processingDetails",
            ))
            .and(matchers::header(
                "authorization",
                "Bearer synthetic-access-token",
            ))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .up_to_n_times(reads)
            .expect(reads)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn youtube_unverified_video_stays_pending_and_reconciles_without_another_upload() {
        let fixture = Fixture::new("youtube-pending").await;
        fixture.connect_all().await;
        let mut cases = vec![
            (serde_json::json!({"items": []}), 200),
            (serde_json::json!({"items": null}), 200),
            (serde_json::json!({"error": "unavailable"}), 503),
        ];
        for (pointer, value) in [
            ("/items/0/id", "another-video"),
            ("/items/0/snippet/channelId", "another-channel"),
            ("/items/0/status/privacyStatus", "private"),
            ("/items/0/status/privacyStatus", "unlisted"),
            ("/items/0/status/uploadStatus", "uploaded"),
            ("/items/0/status/uploadStatus", "rejected"),
            ("/items/0/processingDetails/processingStatus", "processing"),
            ("/items/0/processingDetails/processingStatus", "failed"),
        ] {
            let mut body = youtube_public_video();
            *body.pointer_mut(pointer).unwrap() = value.into();
            cases.push((body, 200));
        }
        for (body, status) in cases {
            let server = MockServer::start().await;
            mount_youtube_upload(&server).await;
            mount_youtube_read(&server, body.clone(), status, 2).await;
            mount_youtube_read(&server, youtube_public_video(), 200, 1).await;
            let publisher = fixture.publisher(&server, allowed());
            let id = fixture.blog_post().await;
            let target = destination(SocialProvider::Youtube);
            for _ in 0..2 {
                let outcome = drive(&publisher, id, &post(true), &target).await;
                assert_eq!(outcome.state, "confirmed");
                let row = fixture.publication(id).await;
                assert_eq!(row.provider_post_id.as_deref(), Some("youtube-video-1"));
                assert_eq!(row.permalink, None, "unverified response: {body}");
                assert_eq!(
                    row.last_error.as_deref(),
                    Some(store::social_publications::PERMALINK_PENDING)
                );
            }
            drive(&publisher, id, &post(true), &target).await;
            let row = fixture.publication(id).await;
            assert_eq!(
                row.permalink.as_deref(),
                Some("https://www.youtube.com/watch?v=youtube-video-1")
            );
            assert_eq!(row.last_error, None);
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn youtube_periodic_readback_recovers_a_published_post_without_resending() {
        let fixture = Fixture::new("youtube-periodic").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        mount_youtube_upload(&server).await;
        mount_youtube_read(&server, youtube_public_video(), 200, 1).await;
        let publisher = fixture.publisher(&server, allowed());
        let id = fixture.blog_post().await;
        let target = destination(SocialProvider::Youtube);
        let sent = send(&publisher, id, &post(true), &target).await;
        assert_eq!(sent.link_pending.as_deref(), Some("youtube-video-1"));
        fixture
            .db
            .query("UPDATE $id SET status = 'published'")
            .bind(("id", store::surreal::record_id("post", id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        // No frozen account selection means the reconciler cannot borrow another connection.
        assert_eq!(publisher.reconcile_youtube_links().await.unwrap(), 0);
        let account = store::social_accounts::connected_account(
            &fixture.db,
            "synthetic-brand",
            SocialProvider::Youtube,
            "youtube-channel",
        )
        .await
        .unwrap()
        .unwrap();
        fixture
            .db
            .query("UPDATE $id SET selected_social_accounts = $accounts")
            .bind(("id", store::surreal::record_id("post", id)))
            .bind(("accounts", vec![account.id]))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(
            fixture
                .publisher(&server, denied_by(Some("dev"), None))
                .reconcile_youtube_links()
                .await
                .unwrap(),
            0
        );
        assert_eq!(publisher.reconcile_youtube_links().await.unwrap(), 1);
        assert_eq!(publisher.reconcile_youtube_links().await.unwrap(), 0);
        assert_eq!(
            fixture.publication(id).await.permalink.as_deref(),
            Some("https://www.youtube.com/watch?v=youtube-video-1")
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn youtube_resumable_upload_returns_confirmed_video_id() {
        let fixture = Fixture::new("youtube").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        mount_youtube_upload(&server).await;
        mount_youtube_read(&server, youtube_public_video(), 200, 1).await;
        let post_id = fixture.blog_post().await;
        let outcome = drive(
            &fixture.publisher(&server, allowed()),
            post_id,
            &post(true),
            &destination(SocialProvider::Youtube),
        )
        .await;
        assert_eq!(outcome.state, "confirmed");
        assert_eq!(
            fixture
                .publication(post_id)
                .await
                .provider_post_id
                .as_deref(),
            Some("youtube-video-1")
        );
        assert_eq!(
            fixture.publication(post_id).await.permalink.as_deref(),
            Some("https://www.youtube.com/watch?v=youtube-video-1")
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn publish_destination_fails_when_media_is_missing() {
        let fixture = Fixture::new("missing-media").await;
        fixture.connect_all().await;
        let server = silent_provider().await;
        let mut missing = post(true);
        missing.video_asset = Some("synthetic/missing.mp4".into());
        for provider in [SocialProvider::Facebook, SocialProvider::Instagram] {
            let post_id = fixture.blog_post().await;
            let outcome = drive(
                &fixture.publisher(&server, allowed()),
                post_id,
                &missing,
                &destination(provider),
            )
            .await;
            assert_eq!(outcome.state, "failed");
        }
        server.verify().await;
    }

    #[test]
    fn captions_tag_brand_links_with_the_platform_as_ref() {
        use super::tag_brand_links;
        let cases = [
            (
                "Read more: https://www.neonlaw.com/blog/wills.",
                SocialProvider::Linkedin,
                "Read more: https://www.neonlaw.com/blog/wills?ref=linkedin.",
            ),
            (
                "(https://neonlaw.com/contact?topic=estate) and http://vestaestateplanning.com",
                SocialProvider::Facebook,
                "(https://neonlaw.com/contact?topic=estate&ref=facebook) and http://vestaestateplanning.com/?ref=facebook",
            ),
            // Someone else's site, an author-chosen source, and plain text stay as written.
            (
                "See https://example.com/x and https://www.neonlaw.com/?utm_source=newsletter",
                SocialProvider::X,
                "See https://example.com/x and https://www.neonlaw.com/?utm_source=newsletter",
            ),
            ("No links here.", SocialProvider::Threads, "No links here."),
        ];
        for (text, provider, want) in cases {
            assert_eq!(tag_brand_links(text, provider), want, "{text}");
        }
    }

    #[test]
    fn caption_falls_back_to_the_post_description() {
        let post = post(false);
        assert_eq!(
            super::caption(&post, SocialProvider::Facebook),
            "Facebook copy"
        );
        let mut untitled = post;
        untitled.facebook_caption = None;
        untitled.instagram_caption = None;
        untitled.threads_caption = None;
        untitled.youtube_caption = None;
        for provider in [
            SocialProvider::Facebook,
            SocialProvider::Instagram,
            SocialProvider::Threads,
            SocialProvider::Youtube,
        ] {
            assert_eq!(super::caption(&untitled, provider), "Synthetic description");
        }
        assert_eq!(
            publication_outcome(&[PublishOutcome::new(SocialProvider::Facebook, "confirmed")]),
            "published"
        );
        assert_eq!(
            publication_outcome(&[PublishOutcome::new(SocialProvider::Facebook, "unknown")]),
            "reconciliation_required"
        );
    }

    #[tokio::test]
    async fn simulated_development_and_invalid_deployments_make_zero_provider_calls() {
        let fixture = Fixture::new("egress").await;
        fixture.connect_all().await;
        let server = silent_provider().await;
        for (gate, reason) in [
            (
                denied_by(Some("production"), Some("true")),
                SocialEgressDenied::SimulatedMatters,
            ),
            (
                denied_by(Some("dev"), None),
                SocialEgressDenied::NotProduction,
            ),
            (
                denied_by(None, Some("false")),
                SocialEgressDenied::NotProduction,
            ),
            (
                denied_by(Some("staging"), None),
                SocialEgressDenied::InvalidConfiguration,
            ),
            (
                denied_by(Some("production"), Some("yes")),
                SocialEgressDenied::InvalidConfiguration,
            ),
        ] {
            for provider in [
                SocialProvider::Facebook,
                SocialProvider::Instagram,
                SocialProvider::Threads,
                SocialProvider::Youtube,
                SocialProvider::Linkedin,
                SocialProvider::X,
            ] {
                let post_id = fixture.blog_post().await;
                let outcome = drive(
                    &fixture.publisher(&server, gate.clone()),
                    post_id,
                    &post(true),
                    &destination(provider),
                )
                .await;
                assert_eq!(outcome.state, "blocked");
                let row = fixture.publication(post_id).await;
                assert_eq!(row.state, "failed");
                assert_eq!(row.last_error.as_deref(), Some(reason.to_string().as_str()));
            }
        }
        assert_eq!(
            publication_outcome(&[PublishOutcome::new(SocialProvider::Facebook, "blocked")]),
            "social_publishing_disabled"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn a_workflow_admitted_before_the_flag_changes_is_judged_at_send_time() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let fixture = Fixture::new("flag-flip").await;
        fixture.connect_all().await;
        let simulated = Arc::new(AtomicBool::new(false));
        let gate = {
            let simulated = simulated.clone();
            SocialEgressGate::new(move || {
                if simulated.load(Ordering::SeqCst) {
                    Err(SocialEgressDenied::SimulatedMatters)
                } else {
                    Ok(())
                }
            })
        };
        let server = MockServer::start().await;
        mount_container(&server, 1).await;
        mount_status(&server, "FINISHED", 1).await;
        mount_publish(&server, json(serde_json::json!({"id": "never"})), 0).await;
        let publisher = fixture.publisher(&server, gate);
        let post_id = fixture.blog_post().await;
        let target = destination(SocialProvider::Instagram);
        // Admitted and its container created while egress was allowed.
        assert!(matches!(
            publisher
                .prepare(post_id, &post(true), &target)
                .await
                .unwrap(),
            Step::Continue(())
        ));
        let Step::Continue(container) = publisher
            .create_container(post_id, &post(true), &target)
            .await
            .unwrap()
        else {
            panic!("container was not created");
        };
        simulated.store(true, Ordering::SeqCst);
        let outcome = publisher
            .publish_container(post_id, &post(true), &target, &container)
            .await
            .unwrap();
        assert_eq!(outcome.state, "blocked");
    }

    #[tokio::test]
    async fn deleted_rehomed_or_revoked_destinations_make_zero_provider_calls() {
        let server = silent_provider().await;

        let deleted = Fixture::new("deleted").await;
        deleted.connect_all().await;
        deleted
            .db
            .query("DELETE brand WHERE brand_key = 'synthetic-brand'")
            .await
            .unwrap()
            .check()
            .unwrap();

        let rehomed = Fixture::new("rehomed").await;
        rehomed.connect_all().await;
        let other_admin = store::persons::create(
            &rehomed.db,
            &NewPerson::with_role("Other Admin", "other-admin@example.com", Role::Admin),
        )
        .await
        .unwrap();
        let other_firm = store::firms::create(
            &rehomed.db,
            &NewFirm {
                name: "Other Firm".into(),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(&rehomed.db).await,
                admin_dri_person_id: other_admin.id,
            },
        )
        .await
        .unwrap();
        rehomed
            .db
            .query("UPDATE brand SET firm_id = $firm WHERE brand_key = 'synthetic-brand'")
            .bind(("firm", store::surreal::record_id("firm", other_firm.id)))
            .await
            .unwrap()
            .check()
            .unwrap();

        let revoked = Fixture::new("revoked").await;
        revoked.connect_all().await;
        revoked
            .db
            .query(
                "UPDATE social_account SET status = 'revoked', ciphertext = '', wrapped_dek = ''",
            )
            .await
            .unwrap()
            .check()
            .unwrap();

        for fixture in [&deleted, &rehomed, &revoked] {
            for provider in [SocialProvider::Facebook, SocialProvider::Instagram] {
                let post_id = fixture.blog_post().await;
                let outcome = drive(
                    &fixture.publisher(&server, allowed()),
                    post_id,
                    &post(true),
                    &destination(provider),
                )
                .await;
                assert_eq!(outcome.state, "failed");
                assert_eq!(
                    fixture.publication(post_id).await.last_error.as_deref(),
                    Some("The connected account is no longer available to this Brand")
                );
            }
        }
        server.verify().await;
    }

    async fn mount_read(server: &MockServer, receipt: &str, field: &str, link: &str) {
        Mock::given(matchers::method("GET"))
            .and(matchers::path(format!("/{receipt}")))
            .and(matchers::query_param("fields", field))
            .respond_with(json(serde_json::json!({field: link, "id": receipt})))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn only_a_provider_returned_permalink_becomes_the_public_link() {
        let fixture = Fixture::new("permalinks").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        // Facebook: the feed edge returns only an opaque receipt.
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/facebook-page/feed"))
            .respond_with(json(serde_json::json!({"id": "123_456"})))
            .expect(1)
            .mount(&server)
            .await;
        mount_read(
            &server,
            "123_456",
            "permalink_url",
            "https://www.facebook.com/synthetic/posts/pfbid0Synthetic",
        )
        .await;
        // Instagram: a Reel's media ID is not its shortcode.
        mount_container(&server, 1).await;
        mount_status(&server, "FINISHED", 1).await;
        mount_publish(
            &server,
            json(serde_json::json!({"id": "17890000000000001"})),
            1,
        )
        .await;
        mount_read(
            &server,
            "17890000000000001",
            "permalink",
            "https://www.instagram.com/reel/SyntheticCode/",
        )
        .await;
        // Threads: a text post through its container.
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/threads-user/threads"))
            .respond_with(json(serde_json::json!({"id": "threads-container"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/threads-container"))
            .respond_with(json(serde_json::json!({"status": "FINISHED"})))
            .mount(&server)
            .await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/threads-user/threads_publish"))
            .respond_with(json(serde_json::json!({"id": "18000000000000002"})))
            .expect(1)
            .mount(&server)
            .await;
        mount_read(
            &server,
            "18000000000000002",
            "permalink",
            "https://www.threads.com/@synthetic/post/SyntheticShort",
        )
        .await;

        let publisher = fixture.publisher(&server, allowed());
        let post_id = fixture.blog_post().await;
        for (provider, video, receipt, link) in [
            (
                SocialProvider::Facebook,
                false,
                "123_456",
                "https://www.facebook.com/synthetic/posts/pfbid0Synthetic",
            ),
            (
                SocialProvider::Instagram,
                true,
                "17890000000000001",
                "https://www.instagram.com/reel/SyntheticCode/",
            ),
            (
                SocialProvider::Threads,
                false,
                "18000000000000002",
                "https://www.threads.com/@synthetic/post/SyntheticShort",
            ),
        ] {
            let outcome = drive(&publisher, post_id, &post(video), &destination(provider)).await;
            assert_eq!(outcome.state, "confirmed");
            let rows = store::social_publications::for_posts(&fixture.db, &[post_id])
                .await
                .unwrap();
            let row = rows
                .iter()
                .find(|row| row.provider == provider.as_str())
                .unwrap();
            assert_eq!(row.provider_post_id.as_deref(), Some(receipt));
            assert_eq!(row.permalink.as_deref(), Some(link));
            assert!(!row.permalink.as_deref().unwrap().contains(receipt));
            assert_eq!(row.last_error, None);
        }
    }

    #[tokio::test]
    async fn a_failed_link_lookup_stays_pending_and_never_resends_the_post() {
        let fixture = Fixture::new("pending-link").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/facebook-page/feed"))
            .respond_with(json(serde_json::json!({"id": "123_789"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/123_789"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        mount_read(
            &server,
            "123_789",
            "permalink_url",
            "/synthetic/videos/123789/",
        )
        .await;
        let publisher = fixture.publisher(&server, allowed());
        let post_id = fixture.blog_post().await;
        let target = destination(SocialProvider::Facebook);

        let first = drive(&publisher, post_id, &post(false), &target).await;
        assert_eq!(first.state, "confirmed");
        let pending = fixture.publication(post_id).await;
        assert_eq!(pending.state, "confirmed");
        assert_eq!(pending.provider_post_id.as_deref(), Some("123_789"));
        assert_eq!(pending.permalink, None);
        assert_eq!(
            pending.last_error.as_deref(),
            Some(store::social_publications::PERMALINK_PENDING)
        );

        // A later run reconciles the link only: the feed is not posted again.
        let rerun = drive(&publisher, post_id, &post(false), &target).await;
        assert_eq!(rerun.state, "confirmed");
        assert_eq!(rerun.link_pending.as_deref(), Some("123_789"));
        let linked = fixture.publication(post_id).await;
        assert_eq!(
            linked.permalink.as_deref(),
            Some("https://www.facebook.com/synthetic/videos/123789/")
        );
        assert_eq!(linked.last_error, None);
    }

    #[tokio::test]
    async fn rate_limits_and_temporary_errors_are_retryable_but_terminal_refusals_are_not() {
        let fixture = Fixture::new("refusals").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        let publisher = fixture.publisher(&server, allowed());
        for (body, status, retryable, prefix) in [
            (
                serde_json::json!({"error": {"message": "Application request limit reached", "code": 4}}),
                400,
                true,
                "Rate limited by the provider; nothing was published. Retry later",
            ),
            (
                serde_json::json!({"error": {"message": "Too many calls"}}),
                429,
                true,
                "Rate limited by the provider; nothing was published. Retry later",
            ),
            (
                serde_json::json!({"error": {"message": "Please retry", "code": 2, "is_transient": true}}),
                400,
                true,
                "The provider reported a temporary failure; nothing was published. Retry later",
            ),
            (
                serde_json::json!({"error": {"message": "Invalid parameter", "code": 100}}),
                400,
                false,
                "The provider rejected the post",
            ),
        ] {
            server.reset().await;
            Mock::given(matchers::method("POST"))
                .and(matchers::path("/facebook-page/feed"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            let post_id = fixture.blog_post().await;
            let outcome = drive(
                &publisher,
                post_id,
                &post(false),
                &destination(SocialProvider::Facebook),
            )
            .await;
            assert_eq!(outcome.state, "failed");
            assert_eq!(outcome.retryable, retryable, "{prefix}");
            let row = fixture.publication(post_id).await;
            assert!(
                row.last_error.as_deref().unwrap().starts_with(prefix),
                "{:?}",
                row.last_error
            );
            // A refusal is known: nothing was published, so a retry may send.
            assert!(matches!(
                store::social_publications::admit(&fixture.db, post_id, "facebook")
                    .await
                    .unwrap(),
                Admission::Admitted(_)
            ));
            server.verify().await;
        }
    }
    #[tokio::test]
    async fn linkedin_receipt_is_confirmed_once_and_ambiguous_writes_never_resend() {
        let fixture = Fixture::new("linkedin").await;
        fixture.connect_all().await;
        let server = MockServer::start().await;
        let publisher = fixture.publisher(&server, allowed());
        let target = destination(SocialProvider::Linkedin);
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/posts"))
            .and(matchers::header("LinkedIn-Version", "202609"))
            .and(matchers::header("X-Restli-Protocol-Version", "2.0.0"))
            .and(matchers::body_json(serde_json::json!({
                "author": "urn:li:organization:1001", "commentary": "Synthetic description",
                "visibility": "PUBLIC", "distribution": {"feedDistribution": "MAIN_FEED",
                    "targetEntities": [], "thirdPartyDistributionChannels": []},
                "lifecycleState": "PUBLISHED", "isReshareDisabledByAuthor": false
            })))
            .respond_with(
                ResponseTemplate::new(201).insert_header("x-restli-id", "urn:li:share:2001"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let id = fixture.blog_post().await;
        assert_eq!(
            drive(&publisher, id, &post(false), &target).await.state,
            "confirmed"
        );
        assert_eq!(
            drive(&publisher, id, &post(false), &target).await.state,
            "confirmed"
        );
        let rows = store::social_publications::for_posts(&fixture.db, &[id])
            .await
            .unwrap();
        assert_eq!(
            rows[0].provider_post_id.as_deref(),
            Some("urn:li:share:2001")
        );
        assert!(rows[0].permalink.is_none());
        server.verify().await;
        server.reset().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/posts"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        let id = fixture.blog_post().await;
        assert_eq!(
            drive(&publisher, id, &post(false), &target).await.state,
            "unknown"
        );
        assert_eq!(
            drive(&publisher, id, &post(false), &target).await.state,
            "unknown"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn linkedin_video_and_oversized_text_are_refused_before_provider_calls() {
        let fixture = Fixture::new("linkedin-invalid").await;
        fixture.connect_all().await;
        let server = silent_provider().await;
        let publisher = fixture.publisher(&server, allowed());
        let target = destination(SocialProvider::Linkedin);
        let mut long = post(false);
        long.description = "x".repeat(3001);
        for invalid in [post(true), long] {
            let id = fixture.blog_post().await;
            assert_eq!(
                drive(&publisher, id, &invalid, &target).await.state,
                "failed"
            );
        }
        server.verify().await;
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "One synthetic fixture checks confirmed, unknown, and mismatched identities"
    )]
    async fn x_receipts_unknown_outcomes_and_wrong_identity_never_duplicate() {
        let fixture = Fixture::new("x-send").await;
        fixture
            .connect(
                SocialProvider::X,
                "1002",
                SocialProvider::X.required_scopes(),
                None,
            )
            .await;
        let server = MockServer::start().await;
        let publisher = fixture.publisher(&server, allowed());
        let target = Destination {
            provider: SocialProvider::X,
            account_id: "1002".into(),
        };
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/users/me"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data":{"id":"1002"}})),
            )
            .mount(&server)
            .await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/tweets"))
            .and(matchers::body_json(
                serde_json::json!({"text":"Synthetic description"}),
            ))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({"data":{"id":"2002"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/tweets/2002"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data":{"id":"2002","author_id":"1002"}})),
            )
            .mount(&server)
            .await;
        let id = fixture.blog_post().await;
        assert_eq!(
            drive(&publisher, id, &post(false), &target).await.state,
            CONFIRMED
        );
        assert_eq!(
            drive(&publisher, id, &post(false), &target).await.state,
            CONFIRMED
        );
        let rows = store::social_publications::for_posts(&fixture.db, &[id])
            .await
            .unwrap();
        assert_eq!(
            rows[0].permalink.as_deref(),
            Some("https://x.com/i/web/status/2002")
        );
        server.verify().await;
        server.reset().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/users/me"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data":{"id":"1002"}})),
            )
            .mount(&server)
            .await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/tweets"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let id = fixture.blog_post().await;
        assert_eq!(
            drive(&publisher, id, &post(false), &target).await.state,
            UNKNOWN
        );
        assert_eq!(
            drive(&publisher, id, &post(false), &target).await.state,
            UNKNOWN
        );
        server.verify().await;
        server.reset().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/users/me"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data":{"id":"other"}})),
            )
            .mount(&server)
            .await;
        assert_eq!(
            drive(&publisher, fixture.blog_post().await, &post(false), &target)
                .await
                .state,
            FAILED
        );
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "GET"));
    }

    #[test]
    fn x_text_bound_accounts_for_unicode_and_short_urls() {
        assert!(super::x_text_eligible(&"界".repeat(140)));
        assert!(!super::x_text_eligible(&"界".repeat(141)));
        assert!(!super::x_text_eligible(&"https://x.co ".repeat(13)));
        assert!(!super::x_text_eligible(&"x.co,".repeat(13)));
        assert!(!super::x_text_eligible(&"x.co,".repeat(12)));
        assert!(super::x_text_eligible(&"x.co,".repeat(8)));
        assert!(!super::x_text_eligible(&"x.co,".repeat(9)));
    }

    #[tokio::test]
    async fn x_video_and_oversized_text_are_rejected_without_network() {
        let fixture = Fixture::new("x-invalid").await;
        fixture
            .connect(
                SocialProvider::X,
                "1002",
                SocialProvider::X.required_scopes(),
                None,
            )
            .await;
        let server = MockServer::start().await;
        let publisher = fixture.publisher(&server, allowed());
        let target = Destination {
            provider: SocialProvider::X,
            account_id: "1002".into(),
        };
        assert_eq!(
            drive(&publisher, fixture.blog_post().await, &post(true), &target)
                .await
                .state,
            FAILED
        );
        let mut large = post(false);
        large.description = "界".repeat(141);
        assert_eq!(
            drive(&publisher, fixture.blog_post().await, &large, &target)
                .await
                .state,
            FAILED
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

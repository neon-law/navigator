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

use crate::workflow_step_error::WorkflowStepError;
use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use store::config::SocialEgressGate;
use store::social_accounts::SocialProvider;
use uuid::Uuid;
use workflows::social_publish::{send_credential, SendCredential, TokenRefreshConfig};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileRequest {
    pub tick: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub started: usize,
}

impl SocialPublisherService {
    #[must_use]
    pub fn new(
        application_store: ApplicationStore,
        kms: Arc<dyn cloud::RuntimeKms>,
        storage: Arc<dyn cloud::StorageService>,
    ) -> Self {
        Self {
            publisher: Publisher {
                application_store,
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
    application_store: &ApplicationStore,
    post_id: Uuid,
    requested_at: DateTime<Utc>,
) -> Result<Option<PostSnapshot>, SnapshotError> {
    let post = store::posts::find_by_id(application_store, post_id).await?;
    let Some(post) = post else {
        return Ok(None);
    };
    if post.status != "scheduled" || post.publish_at != Some(requested_at) {
        return Ok(None);
    }
    let accounts = store::social_accounts::selected_for_brand(
        application_store,
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

/// Attempts for a destination the provider rate-limited or failed temporarily.
pub(crate) const RETRYABLE_ATTEMPTS: usize = 3;
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(120);
/// How often a `TikTok` upload task is polled while the provider processes it.
pub(crate) const TIKTOK_POLL: Duration = Duration::from_secs(180);
/// A provider can take a moment to expose a new post's public link.
pub(crate) const PERMALINK_READS: usize = 3;
pub(crate) const PERMALINK_READ_AFTER: Duration = Duration::from_secs(30);

pub(crate) const fn provider_name(provider: SocialProvider) -> &'static str {
    provider.as_str()
}

/// Everything a destination step needs. Steps receive no secret through the
/// journal: each resolves its own credential immediately before its request.
#[derive(Clone)]
pub struct Publisher {
    application_store: ApplicationStore,
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
        application_store: ApplicationStore,
        kms: Arc<dyn cloud::RuntimeKms>,
        storage: Arc<dyn cloud::StorageService>,
        egress: SocialEgressGate,
        refresh: TokenRefreshConfig,
        http: reqwest::Client,
        video_url: fn(&str) -> String,
    ) -> Self {
        Self {
            application_store,
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
    pub(crate) fn application_store(&self) -> &ApplicationStore {
        &self.application_store
    }

    pub(crate) async fn start_tiktok(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<Step<String>, WorkflowStepError> {
        let publications =
            store::social_publications::for_posts(&self.application_store, &[post_id])
                .await
                .map_err(WorkflowStepError::retryable)?;
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
                    .map(Step::Settled);
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
    ) -> Result<Step<bool>, WorkflowStepError> {
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
    ) -> Result<Step<SendCredential>, WorkflowStepError> {
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
    ) -> Result<Step<Vec<u8>>, WorkflowStepError> {
        let measured = store::posts::find_video(&self.application_store, key)
            .await
            .map_err(WorkflowStepError::retryable)?;
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
    ) -> Result<Step<u32>, WorkflowStepError> {
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
    ) -> Result<Step<String>, WorkflowStepError> {
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
        store::social_publications::record_provider_task(
            &self.application_store,
            post_id,
            "tiktok",
            task,
        )
        .await
        .map_err(WorkflowStepError::retryable)?;
        Ok(Step::Continue(task.to_string()))
    }

    async fn tiktok_source_matches(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
    ) -> Result<bool, WorkflowStepError> {
        let current = store::posts::find_by_id(&self.application_store, post_id)
            .await
            .map_err(WorkflowStepError::retryable)?;
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
    ) -> Result<Option<PublishOutcome>, WorkflowStepError> {
        let publication =
            store::social_publications::for_posts(&self.application_store, &[post_id])
                .await
                .map_err(WorkflowStepError::retryable)?
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
    ) -> Result<Step<()>, WorkflowStepError> {
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
            &self.application_store,
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
                    &self.application_store,
                    post_id,
                    "tiktok",
                    &ids[0],
                    None,
                )
                .await
                .map_err(WorkflowStepError::retryable)?;
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
    ) -> Result<Step<()>, WorkflowStepError> {
        let provider = destination.provider;
        let admission =
            store::social_publications::admit(&self.application_store, post_id, provider.as_str())
                .await
                .map_err(WorkflowStepError::retryable)?;
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
    ) -> Result<Result<SendCredential, PublishOutcome>, WorkflowStepError> {
        let provider = destination.provider;
        if let Err(denied) = self.egress.check() {
            self.record_before_send(post_id, provider, &denied.to_string())
                .await?;
            return Ok(Err(PublishOutcome::new(provider, BLOCKED)));
        }
        match send_credential(
            &self.application_store,
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
    ) -> Result<(), WorkflowStepError> {
        store::social_publications::mark_failed_before_send(
            &self.application_store,
            post_id,
            provider.as_str(),
            reason,
        )
        .await
        .map(|_| ())
        .map_err(WorkflowStepError::retryable)
    }

    async fn fail_before_send(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        reason: &str,
    ) -> Result<PublishOutcome, WorkflowStepError> {
        self.record_before_send(post_id, provider, reason).await?;
        Ok(PublishOutcome::new(provider, FAILED))
    }

    /// Record a known failure: the provider refused, or nothing was published.
    pub(crate) async fn fail(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        reason: &str,
    ) -> Result<PublishOutcome, WorkflowStepError> {
        store::social_publications::mark_failed(
            &self.application_store,
            post_id,
            provider.as_str(),
            reason,
        )
        .await
        .map_err(WorkflowStepError::retryable)?;
        Ok(PublishOutcome::new(provider, FAILED))
    }

    async fn unknown(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
        reason: &str,
    ) -> Result<PublishOutcome, WorkflowStepError> {
        store::social_publications::mark_unknown(
            &self.application_store,
            post_id,
            provider.as_str(),
            reason,
        )
        .await
        .map_err(WorkflowStepError::retryable)?;
        Ok(PublishOutcome::new(provider, UNKNOWN))
    }

    /// Claim the publication's one public write. `None` means it was already
    /// claimed and the destination needs reconciliation, not another send.
    async fn claim_write(
        &self,
        post_id: Uuid,
        provider: SocialProvider,
    ) -> Result<Option<PublishOutcome>, WorkflowStepError> {
        let claimed = store::social_publications::begin_provider_write(
            &self.application_store,
            post_id,
            provider.as_str(),
        )
        .await
        .map_err(WorkflowStepError::retryable)?;
        Ok((!claimed).then(|| PublishOutcome::new(provider, UNKNOWN)))
    }

    /// Create an Instagram Reel or Threads media container. A container is not
    /// public, so any failure here is known: nothing was published.
    pub(crate) async fn create_container(
        &self,
        post_id: Uuid,
        post: &PostSnapshot,
        destination: &Destination,
    ) -> Result<Step<String>, WorkflowStepError> {
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
    ) -> Result<Step<ContainerStatus>, WorkflowStepError> {
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
    ) -> Result<PublishOutcome, WorkflowStepError> {
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
    ) -> Result<PublishOutcome, WorkflowStepError> {
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
    ) -> Result<PublishOutcome, WorkflowStepError> {
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
                    .await;
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
    ) -> Result<PublishOutcome, WorkflowStepError> {
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
    ) -> Result<bool, WorkflowStepError> {
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
        store::social_publications::record_permalink(
            &self.application_store,
            post_id,
            "tiktok",
            link,
        )
        .await
        .map(|_| true)
        .map_err(WorkflowStepError::retryable)
    }

    /// Revisit retained uploads after the initial polling window. This path only reads providers.
    pub(crate) async fn reconcile_youtube_links(&self) -> Result<usize, WorkflowStepError> {
        if self.egress.check().is_err() {
            return Ok(0);
        }
        let pending =
            store::social_publications::pending_links(&self.application_store, "youtube", 100)
                .await
                .map_err(WorkflowStepError::retryable)?;
        let mut recorded = 0;
        for publication in pending {
            let Some(receipt) = publication.provider_post_id.as_deref() else {
                continue;
            };
            let Some(post) = store::posts::find_by_id(&self.application_store, publication.post_id)
                .await
                .map_err(WorkflowStepError::retryable)?
            else {
                continue;
            };
            let accounts = store::social_accounts::selected_for_brand(
                &self.application_store,
                &post.brand_key,
                &post.selected_social_accounts,
            )
            .await
            .map_err(WorkflowStepError::retryable)?;
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
    ) -> Result<bool, WorkflowStepError> {
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
            &self.application_store,
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
    ) -> Result<bool, WorkflowStepError> {
        let provider = destination.provider;
        if self.egress.check().is_err() {
            return Ok(false);
        }
        let Ok(credential) = send_credential(
            &self.application_store,
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
                &self.application_store,
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
            &self.application_store,
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
    ) -> Result<PublishOutcome, WorkflowStepError> {
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
                    &self.application_store,
                    post_id,
                    provider.as_str(),
                    id,
                    permalink.as_deref(),
                )
                .await
                .map_err(WorkflowStepError::retryable)?;
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
        .any(|key| key.apex() == Some(host) || key.hosts().contains(&host));
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

    use super::{container_policy, publication_outcome, PostSnapshot, PublishOutcome};
    use chrono::Utc;

    use store::social_accounts::SocialProvider;

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

    #[test]
    fn refusals_are_classified_from_status_and_provider_error_codes() {
        use super::{refusal_kind, RefusalKind};
        use reqwest::StatusCode;
        let cases = [
            (StatusCode::UNAUTHORIZED, None, RefusalKind::Authorization),
            (
                StatusCode::BAD_REQUEST,
                Some(serde_json::json!({"code": 190})),
                RefusalKind::Authorization,
            ),
            (
                StatusCode::BAD_REQUEST,
                Some(serde_json::json!({"code": 102})),
                RefusalKind::Authorization,
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                None,
                RefusalKind::RateLimited,
            ),
            (
                StatusCode::BAD_REQUEST,
                Some(serde_json::json!({"code": 613})),
                RefusalKind::RateLimited,
            ),
            (
                StatusCode::BAD_REQUEST,
                Some(serde_json::json!({"code": 80_005})),
                RefusalKind::RateLimited,
            ),
            (
                StatusCode::FORBIDDEN,
                Some(serde_json::json!({"errors": [{"reason": "quotaExceeded"}]})),
                RefusalKind::RateLimited,
            ),
            (
                StatusCode::BAD_REQUEST,
                Some(serde_json::json!({"code": 1, "is_transient": false})),
                RefusalKind::Temporary,
            ),
            (
                StatusCode::BAD_REQUEST,
                Some(serde_json::json!({"code": 100, "is_transient": true})),
                RefusalKind::Temporary,
            ),
            (
                StatusCode::BAD_REQUEST,
                Some(serde_json::json!({"code": 100})),
                RefusalKind::Rejected,
            ),
            (
                StatusCode::FORBIDDEN,
                Some(serde_json::json!({"errors": [{"reason": "forbidden"}]})),
                RefusalKind::Rejected,
            ),
            (StatusCode::NOT_FOUND, None, RefusalKind::Rejected),
        ];
        for (status, error, want) in cases {
            assert_eq!(
                refusal_kind(status, error.as_ref()),
                want,
                "{status} {error:?}"
            );
        }
    }

    #[test]
    fn refusal_reasons_name_the_retry_posture_and_stay_bounded() {
        use super::{ProviderRefusal, RefusalKind};
        let refusal = |kind| ProviderRefusal {
            kind,
            message: "synthetic detail".into(),
        };
        assert_eq!(
            refusal(RefusalKind::Authorization).reason(SocialProvider::Facebook),
            "Reconnect the facebook account: the provider rejected its authorization"
        );
        assert!(refusal(RefusalKind::RateLimited)
            .reason(SocialProvider::Youtube)
            .starts_with("Rate limited by the provider; nothing was published"));
        assert!(refusal(RefusalKind::Temporary)
            .reason(SocialProvider::Threads)
            .contains("temporary failure"));
        assert_eq!(
            ProviderRefusal::rejected("synthetic detail").reason(SocialProvider::X),
            "The provider rejected the post: synthetic detail"
        );
        let long = ProviderRefusal::rejected(&"x".repeat(1000));
        assert_eq!(long.reason(SocialProvider::X).chars().count(), 300);
        assert_eq!(super::bounded(&"界".repeat(301)).chars().count(), 300);
    }

    #[test]
    fn only_https_provider_returned_links_become_permalinks() {
        use super::provider_permalink;
        let body = |field: &str, value: &str| serde_json::json!({ field: value });
        assert_eq!(
            provider_permalink(
                SocialProvider::Facebook,
                &body("permalink_url", "/synthetic/videos/1")
            )
            .as_deref(),
            Some("https://www.facebook.com/synthetic/videos/1")
        );
        assert_eq!(
            provider_permalink(
                SocialProvider::Facebook,
                &body("permalink_url", "https://www.facebook.com/p/1")
            )
            .as_deref(),
            Some("https://www.facebook.com/p/1")
        );
        assert!(provider_permalink(
            SocialProvider::Facebook,
            &body("permalink_url", "http://www.facebook.com/p/1")
        )
        .is_none());
        assert!(provider_permalink(SocialProvider::Instagram, &body("permalink", "   ")).is_none());
        assert_eq!(
            provider_permalink(
                SocialProvider::Threads,
                &body("permalink", " https://www.threads.net/@s/post/1 ")
            )
            .as_deref(),
            Some("https://www.threads.net/@s/post/1")
        );
        // A receipt-only provider never turns a body field into a public link.
        for provider in [
            SocialProvider::Linkedin,
            SocialProvider::X,
            SocialProvider::Tiktok,
            SocialProvider::Youtube,
        ] {
            assert!(provider_permalink(
                provider,
                &serde_json::json!({"permalink": "https://example.test/1", "permalink_url": "https://example.test/1"})
            )
            .is_none());
        }
    }

    #[test]
    fn publication_outcome_prefers_failure_then_reconciliation_and_names_a_disabled_deployment() {
        let outcome = |state: &str| PublishOutcome::new(SocialProvider::Facebook, state);
        assert_eq!(
            publication_outcome(&[outcome("blocked"), outcome("blocked")]),
            "social_publishing_disabled"
        );
        assert_eq!(publication_outcome(&[]), "published");
        assert_eq!(
            publication_outcome(&[outcome(UNKNOWN), outcome(FAILED)]),
            "partial_failure"
        );
        assert_eq!(
            publication_outcome(&[outcome("blocked"), outcome(CONFIRMED)]),
            "published"
        );
        let labels = [
            SocialProvider::Facebook,
            SocialProvider::Instagram,
            SocialProvider::Threads,
            SocialProvider::Youtube,
            SocialProvider::Linkedin,
            SocialProvider::X,
            SocialProvider::Tiktok,
        ]
        .map(super::provider_label);
        assert_eq!(
            labels,
            [
                "Facebook",
                "Instagram",
                "Threads",
                "YouTube",
                "LinkedIn",
                "X",
                "TikTok"
            ]
        );
        let instagram = container_policy(SocialProvider::Instagram).unwrap();
        assert_eq!(
            (instagram.interval, instagram.checks),
            (Duration::from_secs(60), 5)
        );
        let threads = container_policy(SocialProvider::Threads).unwrap();
        assert_eq!(
            (threads.interval, threads.checks),
            (Duration::from_secs(30), 10)
        );
        assert!(container_policy(SocialProvider::Facebook).is_none());
    }
}

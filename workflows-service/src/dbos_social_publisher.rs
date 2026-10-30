//! DBOS port of the social publication workflows.
//!
//! [`SocialPublisher`] and [`SocialPublishReconciler`] run on Restate today;
//! this module journals the same calls as DBOS steps and replaces Restate's
//! durable `ctx.sleep` with `dbos::sleep`, whose wake time is checkpointed
//! before the first wait — a worker that dies mid-poll resumes at the same
//! step with the original deadline instead of re-waiting. The provider
//! interactions and their store claims stay in `social_publisher.rs`: both
//! engines drive one [`Publisher`], so a step body here and a Restate
//! `ctx.run` there call the identical code.
//!
//! Two layers keep a provider write from happening twice:
//!
//! 1. Every provider interaction is a journaled step, so DBOS serves a
//!    committed step from its journal on replay instead of running it again.
//! 2. Inside the step, the provider's one public write is claimed in the
//!    store (`store::social_publications::begin_provider_write`, keyed by the
//!    publication's `(post, provider)` identity) before the request. A step
//!    that times out after the provider accepted the write re-runs into that
//!    claim and settles `unknown` — the post is never sent again, and the
//!    reconciler closes the loop from the publication state.
//!
//! The rest of Restate's durable surface carries over step by step: the
//! `publish_at` timer, the retryable-backoff timer, the permalink read-after
//! timer, the container-readiness poll, and the `TikTok` upload poll are all
//! durable timers; the reconciler starts one publisher run per due post as a
//! child workflow with the deterministic id
//! [`publisher_workflow_id`], and its own slot starts are idempotent through
//! [`reconciler_workflow_id`]. Steps retry transient failures a bounded
//! number of times and then fail the run; Restate Cloud retried a failed
//! invocation indefinitely, so that resilience difference is a documented
//! tradeoff of this port (recovery is the reconciler's publication-state
//! machinery plus a manual resume).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows::{SocialPublishReport, SocialPublishRequest};

use crate::social_publisher::{
    container_policy, provider_name, publication_outcome, scheduled_post_snapshot, ContainerPolicy,
    ContainerStatus, Destination, PostSnapshot, PublishOutcome, Publisher, ReconcileReport,
    ReconcileRequest, Step, FAILED, PERMALINK_READS, PERMALINK_READ_AFTER, RETRYABLE_ATTEMPTS,
    RETRY_AFTER, TIKTOK_POLL, UNKNOWN,
};
use store::social_accounts::SocialProvider;

pub const PUBLISHER_ENABLED_ENV: &str = "NAVIGATOR_DBOS_SOCIAL_PUBLISHER_ENABLED";
pub const RECONCILER_ENABLED_ENV: &str = "NAVIGATOR_DBOS_SOCIAL_PUBLISH_RECONCILER_ENABLED";
const PUBLISHER_WORKFLOW: &str = "SocialPublisher";
const RECONCILER_WORKFLOW: &str = "SocialPublishReconciler";
/// How often the reconciler wakes to start due posts and recover notices.
const SLOT_SECONDS: u32 = 300;

/// Step retries for reads, store claims, and child starts: three attempts,
/// two seconds apart, doubling. A step that exhausts them fails the run.
const STEP_ATTEMPTS: u32 = 3;
/// Step retries for the provider's one public write: two attempts. A second
/// attempt after the store claim re-runs into the claim and settles
/// `unknown` instead of sending again.
const WRITE_ATTEMPTS: u32 = 2;
const STEP_INTERVAL: Duration = Duration::from_secs(2);
const STEP_BACKOFF: f64 = 2.0;
const STEP_MAX_INTERVAL: Duration = Duration::from_secs(30);

pub type SocialPublishWorkflow =
    WorkflowRef<SocialPublishRequest, SocialPublishReport, SocialPublishDbosError>;
pub type ReconcilerWorkflow =
    WorkflowRef<ReconcileRequest, ReconcileReport, SocialPublishDbosError>;

/// Why a social publication or reconciliation run failed. Phases only — the
/// publication state in the store carries the provider-facing detail.
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum SocialPublishDbosError {
    #[error("the scheduled Post could not be loaded")]
    Snapshot,
    #[error("a provider interaction failed")]
    Provider,
    #[error("a scheduled child workflow could not be started")]
    Dispatch,
    #[error("the YouTube public links could not be reconciled")]
    Links,
}

/// The durable-timer durations this engine waits. Every field defaults to
/// the constant the Restate handler uses; the overrides exist so the
/// worker-death replay tests can hold a container poll inside a second-scale
/// deadline instead of a provider's minute-scale one.
#[derive(Clone, Debug, Default)]
pub struct Pacing {
    tiktok_poll: Option<Duration>,
    retry_after: Option<Duration>,
    permalink_read_after: Option<Duration>,
    container_interval: Option<Duration>,
}

impl Pacing {
    #[must_use]
    pub fn with_tiktok_poll(mut self, duration: Duration) -> Self {
        self.tiktok_poll = Some(duration);
        self
    }

    #[must_use]
    pub fn with_retry_after(mut self, duration: Duration) -> Self {
        self.retry_after = Some(duration);
        self
    }

    #[must_use]
    pub fn with_permalink_read_after(mut self, duration: Duration) -> Self {
        self.permalink_read_after = Some(duration);
        self
    }

    #[must_use]
    pub fn with_container_interval(mut self, duration: Duration) -> Self {
        self.container_interval = Some(duration);
        self
    }

    fn tiktok_poll(&self) -> Duration {
        self.tiktok_poll.unwrap_or(TIKTOK_POLL)
    }

    fn retry_after(&self) -> Duration {
        self.retry_after.unwrap_or(RETRY_AFTER)
    }

    fn permalink_read_after(&self) -> Duration {
        self.permalink_read_after.unwrap_or(PERMALINK_READ_AFTER)
    }

    /// The provider's container-readiness poll, with the interval overridden
    /// when pacing says so. The check count stays the provider's.
    fn container_policy(&self, provider: SocialProvider) -> Option<ContainerPolicy> {
        container_policy(provider).map(|policy| ContainerPolicy {
            interval: self.container_interval.unwrap_or(policy.interval),
            checks: policy.checks,
        })
    }
}

/// The publisher both engines drive, with this engine's timer pacing.
#[derive(Clone)]
pub struct PublisherContext {
    publisher: Publisher,
    pacing: Pacing,
}

impl PublisherContext {
    #[must_use]
    pub fn new(publisher: Publisher) -> Self {
        Self {
            publisher,
            pacing: Pacing::default(),
        }
    }

    #[must_use]
    pub fn with_pacing(mut self, pacing: Pacing) -> Self {
        self.pacing = pacing;
        self
    }
}

/// The reconciler's dependencies: the store it reads due posts and notices
/// from, the publisher its link reconciliation drives, the registered
/// publisher workflow it starts children of, and the Restate ingress the
/// inbound-notice recovery still reaches until `InboundMessageAlert` has its
/// own DBOS twin.
#[derive(Clone)]
pub struct ReconcilerContext {
    surreal: SurrealDb,
    publisher: Publisher,
    social_publisher: SocialPublishWorkflow,
    ingress: Option<String>,
    ingress_token: Option<String>,
    pacing: Pacing,
}

impl ReconcilerContext {
    /// Assembles the context from process configuration. The ingress comes
    /// from the same broker settings the Restate endpoint uses, because the
    /// notice-recovery step calls it.
    pub fn from_lookup(
        surreal: SurrealDb,
        publisher: Publisher,
        social_publisher: SocialPublishWorkflow,
        get: impl Fn(&str) -> Option<String>,
    ) -> Self {
        Self {
            surreal,
            publisher,
            social_publisher,
            ingress: get("RESTATE_BROKER_URL").filter(|url| !url.trim().is_empty()),
            ingress_token: get("RESTATE_AUTH_TOKEN").filter(|token| !token.trim().is_empty()),
            pacing: Pacing::default(),
        }
    }

    #[must_use]
    pub fn with_pacing(mut self, pacing: Pacing) -> Self {
        self.pacing = pacing;
        self
    }
}

pub fn publisher_enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(PUBLISHER_ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_SOCIAL_PUBLISHER_ENABLED must be true or false"),
    }
}

pub fn reconciler_enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(RECONCILER_ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_SOCIAL_PUBLISH_RECONCILER_ENABLED must be true or false"),
    }
}

/// The publisher run's deterministic identity, derived from the same
/// `(post, scheduled time)` pair the Restate ingress keys the invocation
/// with. A re-fire of the same schedule resumes the existing run.
#[must_use]
pub fn publisher_workflow_id(post_id: Uuid, publish_at: DateTime<Utc>) -> String {
    format!("{PUBLISHER_WORKFLOW}:{post_id}:{}", publish_at.timestamp())
}

/// The reconciler slot's deterministic identity.
#[must_use]
pub fn reconciler_workflow_id(slot: DateTime<Utc>) -> String {
    format!("{RECONCILER_WORKFLOW}:{}", slot.format("%Y-%m-%dT%H-%M"))
}

/// The five-minute UTC slot `now` falls in — the reconciler's scheduled-time
/// key, the counterpart of the Restate `CronJob`'s `UTC` invocation key.
#[must_use]
pub fn reconcile_slot(now: DateTime<Utc>) -> DateTime<Utc> {
    let stamp = now.timestamp() - now.timestamp().rem_euclid(i64::from(SLOT_SECONDS));
    DateTime::from_timestamp(stamp, 0).unwrap_or(now)
}

pub fn register_publisher(
    dbos: &DBOS,
    context: PublisherContext,
) -> dbos::Result<SocialPublishWorkflow> {
    dbos.register_workflow(PUBLISHER_WORKFLOW, move |request: SocialPublishRequest| {
        let context = context.clone();
        async move {
            let report = publish_post(&context.publisher, &context.pacing, request).await?;
            Ok(report)
        }
    })
}

pub fn register_reconciler(
    dbos: &DBOS,
    context: ReconcilerContext,
) -> dbos::Result<ReconcilerWorkflow> {
    dbos.register_workflow(RECONCILER_WORKFLOW, move |request: ReconcileRequest| {
        let context = context.clone();
        async move {
            let report = reconcile(&context, request).await?;
            Ok(report)
        }
    })
}

/// The `SocialPublisher` orchestration, step for step against the Restate
/// handler: the `publish_at` timer, the shared scheduled-post load, one
/// destination lane with its retryable backoff and permalink reads, then the
/// post's publication.
async fn publish_post(
    publisher: &Publisher,
    pacing: &Pacing,
    request: SocialPublishRequest,
) -> dbos::Result<SocialPublishReport, SocialPublishDbosError> {
    // The scheduled publish timer: DBOS checkpoints the wake time before the
    // first wait, so a worker replaced mid-wait resumes with the original
    // deadline. A delay that elapsed while the run was queued records the
    // checkpoint and returns.
    let delay = (request.publish_at - Utc::now())
        .to_std()
        .unwrap_or(Duration::ZERO);
    if !delay.is_zero() {
        dbos::sleep::<SocialPublishDbosError>(delay).await?;
    }

    let post_id = request.post_id;
    let surreal = publisher.surreal().clone();
    let snapshot = dbos::step("load-scheduled-post", move || {
        let surreal = surreal.clone();
        async move {
            scheduled_post_snapshot(&surreal, post_id, request.publish_at)
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Snapshot))
        }
    })
    .await?;
    let Some(snapshot) = snapshot else {
        return Ok(SocialPublishReport {
            post_id,
            outcome: "skipped_stale_schedule".into(),
            destinations: 0,
        });
    };

    let mut outcomes = Vec::with_capacity(snapshot.destinations.len());
    for destination in snapshot.destinations.clone() {
        let mut outcome =
            publish_destination(publisher, pacing, post_id, &snapshot, &destination).await?;
        for _ in 1..RETRYABLE_ATTEMPTS {
            if !outcome.retryable {
                break;
            }
            dbos::sleep::<SocialPublishDbosError>(pacing.retry_after()).await?;
            outcome =
                publish_destination(publisher, pacing, post_id, &snapshot, &destination).await?;
        }
        if let Some(receipt) = outcome.link_pending.clone() {
            read_back_permalink(
                publisher,
                pacing,
                post_id,
                &snapshot,
                &destination,
                &receipt,
            )
            .await?;
        }
        outcomes.push(outcome);
    }

    let failed = outcomes.iter().any(|outcome| outcome.state == FAILED);
    let unknown = outcomes.iter().any(|outcome| outcome.state == UNKNOWN);
    if !failed && !unknown {
        mark_post_published(publisher, post_id).await?;
    }

    Ok(SocialPublishReport {
        post_id,
        outcome: publication_outcome(&outcomes).into(),
        destinations: outcomes.len(),
    })
}

/// One destination's journaled steps. A container created by a journaled step
/// is never created again when the run resumes, and the provider's one public
/// write re-runs into its store claim rather than sending again.
async fn publish_destination(
    publisher: &Publisher,
    pacing: &Pacing,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
) -> dbos::Result<PublishOutcome, SocialPublishDbosError> {
    let prepared = prepare_step(publisher, post_id, post, destination).await?;
    if let Step::Settled(outcome) = prepared {
        return Ok(outcome);
    }
    if destination.provider == SocialProvider::Tiktok {
        return publish_tiktok(publisher, pacing, post_id, post, destination).await;
    }
    let Some(policy) = pacing.container_policy(destination.provider) else {
        return send_direct_step(publisher, post_id, post, destination).await;
    };
    let created = create_container_step(publisher, post_id, post, destination).await?;
    let container = match created {
        Step::Continue(container) => container,
        Step::Settled(outcome) => return Ok(outcome),
    };
    for check in 0..policy.checks {
        // The container-readiness timer: checkpointed before each wait.
        dbos::sleep::<SocialPublishDbosError>(policy.interval).await?;
        let status =
            container_status_step(publisher, post_id, post, destination, &container, check).await?;
        match status {
            Step::Settled(outcome) => return Ok(outcome),
            Step::Continue(ContainerStatus::Processing) => {}
            Step::Continue(ContainerStatus::Ready) => {
                return publish_container_step(publisher, post_id, post, destination, &container)
                    .await;
            }
        }
    }
    container_timeout_step(publisher, post_id, destination).await
}

/// `TikTok`'s upload-task poll: submit once, then a durable timer per poll
/// until the provider finishes processing.
async fn publish_tiktok(
    publisher: &Publisher,
    pacing: &Pacing,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
) -> dbos::Result<PublishOutcome, SocialPublishDbosError> {
    let accepted = submit_tiktok_step(publisher, post_id, post, destination).await?;
    let task = match accepted {
        Step::Continue(task) => task,
        Step::Settled(outcome) => return Ok(outcome),
    };
    let mut check = 0_u64;
    loop {
        dbos::sleep::<SocialPublishDbosError>(pacing.tiktok_poll()).await?;
        let polled = poll_tiktok_step(publisher, post_id, post, destination, &task, check).await?;
        if let Step::Settled(outcome) = polled {
            return Ok(outcome);
        }
        check += 1;
    }
}

/// A confirmed post's public link, read back on durable timers until it
/// appears or the reads run out. An unreadable link leaves the publication
/// confirmed with its link pending; the reconciler closes it later.
async fn read_back_permalink(
    publisher: &Publisher,
    pacing: &Pacing,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
    receipt: &str,
) -> dbos::Result<(), SocialPublishDbosError> {
    let step_name = format!("permalink-{}", provider_name(destination.provider));
    for read in 0..PERMALINK_READS {
        if read > 0 {
            dbos::sleep::<SocialPublishDbosError>(pacing.permalink_read_after()).await?;
        }
        let publisher = publisher.clone();
        let (post, target, receipt) = (post.clone(), destination.clone(), receipt.to_owned());
        let recorded = dbos::step(&step_name, move || {
            let publisher = publisher.clone();
            let (post, target, receipt) = (post.clone(), target.clone(), receipt.clone());
            async move {
                publisher
                    .read_permalink(post_id, &post.brand_key, &target, &receipt)
                    .await
                    .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
            }
        })
        .await?;
        if recorded {
            break;
        }
    }
    Ok(())
}

/// The Post's publication, mirrored from the Restate handler: once every
/// destination is confirmed, the Post leaves `scheduled`.
async fn mark_post_published(
    publisher: &Publisher,
    post_id: Uuid,
) -> dbos::Result<(), SocialPublishDbosError> {
    let surreal = publisher.surreal().clone();
    dbos::step("mark-post-published", move || {
        let surreal = surreal.clone();
        async move {
            surreal
                .query(
                    "UPDATE $id SET status = 'published', updated_at = time::now() WHERE status = 'scheduled'",
                )
                .bind(("id", store::surreal::record_id("post", post_id)))
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))?
                .check()
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))?;
            store::postgres_mirror::after_write(
                &surreal,
                store::postgres_mirror::MirrorKey::Post(post_id),
            )
            .await;
            Ok(())
        }
    })
    .await
}

/// The `SocialPublishReconciler` orchestration: start one publisher run per
/// due post, recover pending inbound notices through the Restate ingress
/// until that alert workflow has its own twin, then reconcile retained
/// `YouTube` uploads.
async fn reconcile(
    context: &ReconcilerContext,
    request: ReconcileRequest,
) -> dbos::Result<ReconcileReport, SocialPublishDbosError> {
    let surreal = context.surreal.clone();
    let scheduled = dbos::step("load-scheduled-social-posts", move || {
        let surreal = surreal.clone();
        async move {
            store::social_publications::due_or_future_scheduled(&surreal, 100)
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Snapshot))
        }
    })
    .await?;

    // The Restate reconciler refuses to run at all without broker
    // configuration, because its notice recovery needs it; this port keeps
    // that gate until InboundMessageAlert's twin lands and the caller is
    // rewired away from Restate ingress.
    if context.ingress.is_none() {
        return Err(dbos::Error::Application(SocialPublishDbosError::Dispatch));
    }

    for (index, (post_id, publish_at)) in scheduled.iter().enumerate() {
        start_publisher_child(&context.social_publisher, *post_id, *publish_at, index).await?;
    }

    let surreal = context.surreal.clone();
    let alerts = dbos::step("load-pending-inbound-notices", move || {
        let surreal = surreal.clone();
        async move {
            store::inbox_alerts::awaiting_reconciliation(&surreal, 15)
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Snapshot))
        }
    })
    .await?;
    for (index, message_id) in alerts.iter().enumerate() {
        recover_notice_step(context, *message_id, &request.tick, index).await?;
    }

    let publisher = context.publisher.clone();
    dbos::step("reconcile-youtube-public-links", move || {
        let publisher = publisher.clone();
        async move {
            publisher
                .reconcile_youtube_links()
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Links))
        }
    })
    .await?;

    Ok(ReconcileReport {
        started: scheduled.len() + alerts.len(),
    })
}

/// Starts the publisher run for one due post as a child workflow keyed by the
/// post's schedule. The journaled start plus the deterministic child id make
/// a reconciler replay or a duplicate slot run a no-op.
async fn start_publisher_child(
    social: &SocialPublishWorkflow,
    post_id: Uuid,
    publish_at: DateTime<Utc>,
    index: usize,
) -> dbos::Result<(), SocialPublishDbosError> {
    let id = publisher_workflow_id(post_id, publish_at);
    let social = social.clone();
    dbos::step(
        &format!("start-scheduled-social-publication-{index}"),
        move || {
            let social = social.clone();
            let id = id.clone();
            async move {
                social
                    .start_with(
                        SocialPublishRequest {
                            post_id,
                            publish_at,
                        },
                        dbos::StartOptions {
                            workflow_id: Some(&id),
                            ..dbos::StartOptions::default()
                        },
                    )
                    .await
                    .map(|_| ())
                    .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Dispatch))
            }
        },
    )
    .await
}

/// One inbound notice's recovery, delivered through the Restate ingress while
/// that alert workflow still runs there. The idempotency key pairs the notice
/// with the reconciler's slot, exactly as the Restate handler keys it.
async fn recover_notice_step(
    context: &ReconcilerContext,
    message_id: Uuid,
    tick: &str,
    index: usize,
) -> dbos::Result<(), SocialPublishDbosError> {
    let (ingress, token) = (context.ingress.clone(), context.ingress_token.clone());
    let tick = tick.to_owned();
    dbos::step(
        &format!("recover-inbound-message-notice-{index}"),
        move || {
            let (ingress, token) = (ingress.clone(), token.clone());
            let tick = tick.clone();
            async move {
                let Some(url) = ingress
                    .as_deref()
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                else {
                    return Err(dbos::Error::Application(SocialPublishDbosError::Dispatch));
                };
                workflows::start_workflow(
                    url,
                    token.as_deref(),
                    "InboundMessageAlert",
                    &format!("{message_id}:{tick}"),
                    "run",
                    &workflows::InboundMessageAlertRequest { message_id },
                    true,
                )
                .await
                .map(|_| ())
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Dispatch))
            }
        },
    )
    .await
}

fn retry_options() -> StepOptions<SocialPublishDbosError> {
    StepOptions {
        max_attempts: STEP_ATTEMPTS,
        interval: STEP_INTERVAL,
        backoff_rate: STEP_BACKOFF,
        max_interval: STEP_MAX_INTERVAL,
        ..StepOptions::default()
    }
}

fn write_options() -> StepOptions<SocialPublishDbosError> {
    StepOptions {
        max_attempts: WRITE_ATTEMPTS,
        interval: STEP_INTERVAL,
        backoff_rate: STEP_BACKOFF,
        max_interval: STEP_MAX_INTERVAL,
        ..StepOptions::default()
    }
}

/// The destination's admission and pre-send checks: the publication row is
/// created or resumed here, and a settled result is a known outcome.
async fn prepare_step(
    publisher: &Publisher,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
) -> dbos::Result<Step<()>, SocialPublishDbosError> {
    let name = provider_name(destination.provider);
    let publisher = publisher.clone();
    let (post, target) = (post.clone(), destination.clone());
    dbos::step_with(&format!("prepare-{name}"), retry_options(), move || {
        let publisher = publisher.clone();
        let (post, target) = (post.clone(), target.clone());
        async move {
            publisher
                .prepare(post_id, &post, &target)
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
        }
    })
    .await
}

/// The destination's one public write for `Facebook` and `YouTube`. The claim in
/// `send_direct`'s store path makes a second attempt settle `unknown` rather
/// than send again.
async fn send_direct_step(
    publisher: &Publisher,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
) -> dbos::Result<PublishOutcome, SocialPublishDbosError> {
    let name = provider_name(destination.provider);
    let publisher = publisher.clone();
    let (post, target) = (post.clone(), destination.clone());
    dbos::step_with(&format!("send-direct-{name}"), write_options(), move || {
        let publisher = publisher.clone();
        let (post, target) = (post.clone(), target.clone());
        async move {
            publisher
                .send_direct(post_id, &post, &target)
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
        }
    })
    .await
}

/// Creates the Instagram Reel or Threads container. A container is not
/// public, so the worst a replay creates is an orphaned container.
async fn create_container_step(
    publisher: &Publisher,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
) -> dbos::Result<Step<String>, SocialPublishDbosError> {
    let name = provider_name(destination.provider);
    let publisher = publisher.clone();
    let (post, target) = (post.clone(), destination.clone());
    dbos::step_with(
        &format!("create-container-{name}"),
        write_options(),
        move || {
            let publisher = publisher.clone();
            let (post, target) = (post.clone(), target.clone());
            async move {
                publisher
                    .create_container(post_id, &post, &target)
                    .await
                    .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
            }
        },
    )
    .await
}

async fn container_status_step(
    publisher: &Publisher,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
    container: &str,
    check: usize,
) -> dbos::Result<Step<ContainerStatus>, SocialPublishDbosError> {
    let name = provider_name(destination.provider);
    let publisher = publisher.clone();
    let (post, target, container) = (post.clone(), destination.clone(), container.to_owned());
    dbos::step_with(
        &format!("container-status-{name}-{check}"),
        retry_options(),
        move || {
            let publisher = publisher.clone();
            let (post, target, container) = (post.clone(), target.clone(), container.clone());
            async move {
                publisher
                    .container_status(post_id, &post, &target, &container)
                    .await
                    .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
            }
        },
    )
    .await
}

/// The ready container's publish: the destination's one public write, claimed
/// in the store before the request.
async fn publish_container_step(
    publisher: &Publisher,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
    container: &str,
) -> dbos::Result<PublishOutcome, SocialPublishDbosError> {
    let name = provider_name(destination.provider);
    let publisher = publisher.clone();
    let (post, target, container) = (post.clone(), destination.clone(), container.to_owned());
    dbos::step_with(
        &format!("publish-container-{name}"),
        write_options(),
        move || {
            let publisher = publisher.clone();
            let (post, target, container) = (post.clone(), target.clone(), container.clone());
            async move {
                publisher
                    .publish_container(post_id, &post, &target, &container)
                    .await
                    .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
            }
        },
    )
    .await
}

/// A container that never became ready: a known failure, nothing published.
async fn container_timeout_step(
    publisher: &Publisher,
    post_id: Uuid,
    destination: &Destination,
) -> dbos::Result<PublishOutcome, SocialPublishDbosError> {
    let name = provider_name(destination.provider);
    let publisher = publisher.clone();
    let provider = destination.provider;
    dbos::step(&format!("container-timeout-{name}"), move || {
        let publisher = publisher.clone();
        async move {
            publisher
                .fail(post_id, provider, "still processing")
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
        }
    })
    .await
}

/// `TikTok`'s upload submission: the preflight and the credential are resolved
/// inside the step, and the upload task is started once.
async fn submit_tiktok_step(
    publisher: &Publisher,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
) -> dbos::Result<Step<String>, SocialPublishDbosError> {
    let publisher = publisher.clone();
    let (post, target) = (post.clone(), destination.clone());
    dbos::step_with("submit-tiktok-business-video", write_options(), move || {
        let publisher = publisher.clone();
        let (post, target) = (post.clone(), target.clone());
        async move {
            publisher
                .start_tiktok(post_id, &post, &target)
                .await
                .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
        }
    })
    .await
}

async fn poll_tiktok_step(
    publisher: &Publisher,
    post_id: Uuid,
    post: &PostSnapshot,
    destination: &Destination,
    task: &str,
    check: u64,
) -> dbos::Result<Step<()>, SocialPublishDbosError> {
    let publisher = publisher.clone();
    let (post, target, task) = (post.clone(), destination.clone(), task.to_owned());
    dbos::step_with(
        &format!("poll-tiktok-business-video-{check}"),
        retry_options(),
        move || {
            let publisher = publisher.clone();
            let (post, target, task) = (post.clone(), target.clone(), task.clone());
            async move {
                publisher
                    .poll_tiktok(post_id, &post, &target, &task)
                    .await
                    .map_err(|_| dbos::Error::Application(SocialPublishDbosError::Provider))
            }
        },
    )
    .await
}

/// The reconciler's clock, overridable in tests.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Fires the reconciler once per five-minute UTC slot and stops cleanly on
/// the shutdown token. A slot's first start records the run under
/// [`reconciler_workflow_id`]; re-firing it is a no-op.
pub async fn schedule(workflow: &ReconcilerWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        if stop.is_cancelled() {
            return;
        }
        let now = (clock)();
        let slot = reconcile_slot(now);
        let wait = if now >= slot {
            if let Err(error) = fire(workflow, slot).await {
                tracing::error!(
                    workflow_id = %reconciler_workflow_id(slot),
                    "the DBOS SocialPublishReconciler slot could not start: {error}"
                );
            }
            let fired_at = (clock)();
            (slot + chrono::Duration::seconds(i64::from(SLOT_SECONDS)) - fired_at)
                .to_std()
                .unwrap_or(Duration::from_secs(1))
        } else {
            (slot - now).to_std().unwrap_or(Duration::from_secs(1))
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = stop.cancelled() => return,
        }
    }
}

pub async fn fire(
    workflow: &ReconcilerWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<
    dbos::WorkflowHandle<ReconcileReport, SocialPublishDbosError>,
    SocialPublishDbosError,
> {
    let id = reconciler_workflow_id(slot);
    workflow
        .start_with(
            ReconcileRequest {
                tick: slot.to_rfc3339(),
            },
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..dbos::StartOptions::default()
            },
        )
        .await
}

/// Starts (or resumes) the scheduled publisher run for one Post. The
/// workflow id is deterministic per `(post, publish_at)` — the run's
/// idempotency key — so re-firing an existing run returns it unchanged.
pub async fn fire_publisher(
    workflow: &SocialPublishWorkflow,
    request: SocialPublishRequest,
) -> dbos::Result<
    dbos::WorkflowHandle<SocialPublishReport, SocialPublishDbosError>,
    SocialPublishDbosError,
> {
    let id = publisher_workflow_id(request.post_id, request.publish_at);
    workflow
        .start_with(
            request,
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..dbos::StartOptions::default()
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn social_publish_flags_default_off_and_reject_garbage() {
        assert!(!publisher_enabled(|_| None).unwrap());
        assert!(!publisher_enabled(|_| Some("false".into())).unwrap());
        assert!(publisher_enabled(|_| Some("true".into())).unwrap());
        assert!(!reconciler_enabled(|_| None).unwrap());
        assert!(reconciler_enabled(|_| Some("true".into())).unwrap());
        assert!(publisher_enabled(|_| Some("yes".into())).is_err());
        assert!(reconciler_enabled(|_| Some("yes".into())).is_err());
        assert_eq!(
            PUBLISHER_ENABLED_ENV,
            "NAVIGATOR_DBOS_SOCIAL_PUBLISHER_ENABLED"
        );
        assert_eq!(
            RECONCILER_ENABLED_ENV,
            "NAVIGATOR_DBOS_SOCIAL_PUBLISH_RECONCILER_ENABLED"
        );
    }

    #[test]
    fn publisher_ids_key_the_post_and_schedule() {
        let post_id = Uuid::nil();
        let publish_at = DateTime::parse_from_rfc3339("2026-10-30T12:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        let id = publisher_workflow_id(post_id, publish_at);
        assert_eq!(
            id,
            format!("SocialPublisher:{post_id}:{}", publish_at.timestamp())
        );
        // A different schedule for the same post is a different run.
        let later = publish_at + chrono::Duration::hours(1);
        assert_ne!(publisher_workflow_id(post_id, later), id);
    }

    #[test]
    fn reconciler_slots_are_five_minute_utc_floors() {
        let now = DateTime::parse_from_rfc3339("2026-10-30T12:07:41Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        let slot = reconcile_slot(now);
        assert_eq!(
            slot,
            DateTime::parse_from_rfc3339("2026-10-30T12:05:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc)
        );
        assert_eq!(
            reconciler_workflow_id(slot),
            "SocialPublishReconciler:2026-10-30T12-05"
        );
        // A slot boundary is its own floor.
        assert_eq!(reconcile_slot(slot), slot);
    }
}

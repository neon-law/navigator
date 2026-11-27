//! The `/app/api/integrations/*` doors.
//!
//! Admin-tier operations for a Project's Firm-private Notion page, Slack
//! channel, and shared Google Calendar. The Calendar inspection door reads
//! only a verified synthetic Project Calendar on staging.
//!
//! ## Why its own noun
//!
//! Not `/app/api/projects/{id}/integrations`. The policy rule for the
//! `projects` prefix admits any authenticated caller up to five segments, so
//! a provisioning path nested there would be policy-reachable by a client
//! even though the handler refuses one — the same reason
//! `project-surfaces`, `project-repositories`, and `project-lifecycle` each
//! carry their own noun.
//!
//! ## Why admin-tier
//!
//! `project-surfaces` is the exact sibling operation — create or adopt a
//! Project's external resources — and it is admin-only. Provisioning a
//! Firm-private channel or page is the same kind of act, and `--all` sweeps
//! every Project the login can see, so the gate matches rather than being
//! loosened for convenience.
//!
//! The tier is not the whole gate. Admin tier says the caller provisions
//! external resources at all; `FirmCapability::UseIntegrations` says whose
//! credentials they may spend. Every door resolves that capability against the
//! target Project's owning Firm before a provider is looked up, so an Admin of
//! one Firm cannot reach another Firm's integrations through a Project the
//! visibility lens happens to show them.
//!
//! The gate is [`AdminSession`], an extractor, and not a check in the handler
//! body. Extractors run before the body is deserialized, so a caller outside
//! the tier gets a 403 whatever they sent; a tier check placed after `Json`
//! answers a malformed-body 422 to someone who was never allowed to send a
//! body at all.
//!
//! ## What the response carries
//!
//! Provisioning responses carry outcome slugs. The staging Calendar read
//! carries event summaries and times. Provider coordinates stay on the
//! Project row for the surfaces authorized to display them.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, Mutex};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, FixedOffset, TimeDelta};
use serde::{Deserialize, Serialize};

use crate::api::{AdminSession, ApiError, ApiState};
use crate::integrations::IntegrationError;

/// Per-`(project, provider)` async locks, keyed by
/// [`workflows::IntegrationJob::job_id`] — the same stable id a durable
/// worker would key a replay-safe invocation on, given its first real caller
/// here. Concurrent `ensure` requests for the same Project and provider
/// serialize on this instead of racing find-then-create against each other,
/// which is what turns a double-submitted or double-scheduled `ensure` into
/// two provider resources instead of one.
///
/// This is a single-process guarantee — the boundary a test harness (one
/// `axum::Router` in one process) can actually exercise and assert against.
/// A multi-replica deployment still needs the provider's own idempotent
/// find-then-create (`cloud::ensure_private_page`/`ensure_private_channel`),
/// which every call here already goes through.
static ENSURE_LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

async fn ensure_lock(job_id: String) -> tokio::sync::OwnedMutexGuard<()> {
    let lock = ENSURE_LOCKS
        .lock()
        .expect("ensure-lock registry poisoned")
        .entry(job_id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    lock.lock_owned().await
}

/// Serialize Calendar changes across web replicas as well as local callers.
pub(crate) async fn calendar_lock(
    project_id: uuid::Uuid,
) -> Result<
    (
        tokio::sync::OwnedMutexGuard<()>,
        Option<store::project_calendar::CalendarWriteLock>,
        tokio::sync::SemaphorePermit<'static>,
    ),
    (),
> {
    static WRITERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);
    let local = ensure_lock(format!("calendar:{project_id}")).await;
    let permit = WRITERS.acquire().await.map_err(|_| ())?;
    let lane = store::project_calendar::lock_writes(project_id)
        .await
        .map_err(|_| ())?;
    Ok((local, lane, permit))
}

/// One Project's result. `outcome` is a closed slug; `detail` is present only
/// for the outcomes that name a mechanism the operator has to act on.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectOutcome {
    pub(crate) project_code: String,
    pub(crate) outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar_recorded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_changes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_action: Option<&'static str>,
}

impl ProjectOutcome {
    fn plain(project_code: String, outcome: &'static str) -> Self {
        Self {
            project_code,
            outcome,
            detail: None,
            calendar_recorded: None,
            pending_changes: None,
            provider_reason: None,
            provider_action: None,
        }
    }

    fn with_detail(project_code: String, outcome: &'static str, detail: String) -> Self {
        Self {
            detail: Some(detail),
            ..Self::plain(project_code, outcome)
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct IntegrationReport {
    pub(crate) results: Vec<ProjectOutcome>,
}

/// `{ "project_code": "…", "all": false }`. Exactly one of the two selects
/// the work, so neither a bare body nor both together is accepted: `--all`
/// with a code would leave it ambiguous whether the code narrowed the sweep
/// or was ignored.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectSelector {
    #[serde(default)]
    pub(crate) project_code: Option<String>,
    #[serde(default)]
    pub(crate) all: bool,
}

/// `{ "project_code": "…", "event": "project_opened" }`. `event` is absent
/// for `ensure` and required for `notify`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlackRequest {
    pub(crate) project_code: String,
    #[serde(default)]
    pub(crate) event: Option<String>,
}

fn selector_error() -> ApiError {
    ApiError::MalformedBody("provide either project_code or all, not both".to_string())
}

/// Resolve the selector to the matters this login may act on.
///
/// Two layers, and both are needed. `store::access::visible_projects` is the
/// same scoping the list door uses, so an admin sweep can never reach a matter
/// the read surface would hide. A single-code door then asks
/// [`authorize_project`] whether this caller may spend the owning Firm's
/// integrations. The `--all` path uses the batch counterpart instead, so the
/// capability read and its telemetry happen once per sweep; visibility is not
/// that permission, so a Project only the lens admits is dropped rather than
/// provisioned.
async fn targets(
    state: &ApiState,
    authed: &AdminSession,
    selector: &ProjectSelector,
) -> Result<Vec<store::projects::Project>, ApiError> {
    match (&selector.project_code, selector.all) {
        (Some(code), false) => Ok(vec![one_target(state, authed, code).await?]),
        (None, true) => {
            let visible = store::access::visible_projects(
                &state.application_store,
                authed.0.person_id,
                authed.0.role,
            )
            .await
            .map_err(ApiError::Db)?;
            let admitted_firm_ids = store::firm_capability::allowed_firm_ids(
                &state.application_store,
                authed.0.role,
                authed.0.person_id,
                store::firm_capability::FirmCapability::UseIntegrations,
            )
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
            let admitted_firm_ids: BTreeSet<_> = admitted_firm_ids.into_iter().collect();
            let visible_project_count = visible.len();
            let authorized: Vec<_> = visible
                .into_iter()
                .filter(|project| {
                    project
                        .firm_id
                        .is_some_and(|firm_id| admitted_firm_ids.contains(&firm_id))
                })
                .collect();
            tracing::info!(
                target: "firm_capability.sweep",
                capability = "use_integrations",
                person_id = authed.0.person_id.map(|id| id.to_string()),
                admitted_firm_count = admitted_firm_ids.len(),
                visible_project_count,
                selected_project_count = authorized.len(),
                "firm capability sweep",
            );
            Ok(authorized)
        }
        _ => Err(selector_error()),
    }
}

async fn one_target(
    state: &ApiState,
    authed: &AdminSession,
    code: &str,
) -> Result<store::projects::Project, ApiError> {
    let person = authed.0.person_id.ok_or(ApiError::NotFound)?;
    let project =
        store::access::visible_firm_project_by_code(&state.application_store, person, code)
            .await
            .map_err(ApiError::Db)?
            .ok_or(ApiError::NotFound)?;
    authorize_project(state, authed, project).await
}

/// Whether this caller may use the owning Firm's integrations for `project`.
///
/// Runs before any provider lookup, so a refusal costs no credential
/// resolution and no provider call. A Project whose Firm denies the caller and
/// a Project with no owning Firm both collapse to `NotFound`, the same answer a
/// code nobody can see gets: which of the three it was is a Firm-boundary fact
/// the caller is not owed.
async fn authorize_project(
    state: &ApiState,
    authed: &AdminSession,
    project: store::projects::Project,
) -> Result<store::projects::Project, ApiError> {
    let Some(firm_id) = project.firm_id else {
        return Err(ApiError::NotFound);
    };
    let decision = store::firm_capability::resolve(
        &state.application_store,
        authed.0.role,
        authed.0.person_id,
        firm_id,
        store::firm_capability::FirmCapability::UseIntegrations,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    if decision.is_allowed() {
        Ok(project)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `POST /app/api/integrations/notion/ensure` — create or adopt each
/// selected Project's Firm-private page and record its address.
pub(crate) async fn notion_ensure_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(selector): Json<ProjectSelector>,
) -> Result<Response, ApiError> {
    let mut results = Vec::new();
    for project in targets(&state, &authed, &selector).await? {
        results.push(ensure_one_page(&state, &project).await);
    }
    Ok(report(results))
}

async fn ensure_one_page(state: &ApiState, project: &store::projects::Project) -> ProjectOutcome {
    let _guard =
        ensure_lock(workflows::IntegrationJob::notion(project.id, &project.code).job_id).await;
    let notion = match state
        .integration_providers
        .notion(&state.application_store, project.id)
        .await
    {
        Ok(notion) => notion,
        Err(error) => return unavailable(project, &error),
    };
    // A recorded page is validated by its own id, never rediscovered by
    // title search: a search cannot see a page after it was renamed away
    // from the recorded title or archived, so treating "the search found
    // nothing" as "nothing was ever provisioned" would create a duplicate
    // beside a page that still exists. Only a genuinely unrecorded or
    // unresolvable coordinate falls through to create-or-adopt below.
    if let Some(recorded_url) = project.private_notion_page_url.clone() {
        if let Some(page_id) = cloud::notion_page_id_from_url(&recorded_url) {
            match notion.get_page(&page_id).await {
                Ok(Some(page)) => {
                    if !notion.is_expected_parent(&page) {
                        return ProjectOutcome::plain(project.code.clone(), "wrong_parent");
                    }
                    let snapshot = to_snapshot(page);
                    let decision = cloud::reconcile_notion_project(
                        &cloud::NotionProjectInput {
                            project_code: project.code.clone(),
                            canonical_url: recorded_url,
                        },
                        std::slice::from_ref(&snapshot),
                    );
                    return apply_notion_decision(
                        state,
                        project,
                        notion.as_ref(),
                        decision,
                        "adopted",
                    )
                    .await;
                }
                Ok(None) => {
                    return ProjectOutcome::plain(project.code.clone(), "recorded_resource_missing")
                }
                Err(_) => {
                    return ProjectOutcome::plain(project.code.clone(), "provider_unavailable")
                }
            }
        }
        return ProjectOutcome::plain(project.code.clone(), "recorded_resource_missing");
    }
    let (page, created) = match cloud::ensure_private_page(notion.as_ref(), &project.code).await {
        Ok(result) => result,
        Err(cloud::NotionError::WrongParent) => {
            return ProjectOutcome::plain(project.code.clone(), "wrong_parent")
        }
        Err(_) => return ProjectOutcome::plain(project.code.clone(), "provider_unavailable"),
    };
    // Record the address before reporting success. The row is what every
    // later surface reads; an ensure that provisioned a page and lost its
    // address would look done and leave the next run creating another.
    if store::projects::set_private_notion_page_url(&state.application_store, project.id, &page.url)
        .await
        .is_err()
    {
        return ProjectOutcome::plain(project.code.clone(), "address_not_recorded");
    }
    ProjectOutcome::plain(
        project.code.clone(),
        if created { "created" } else { "adopted" },
    )
}

/// Map a reconcile decision to the reported outcome, applying the one
/// decision that is a repair. Shared by `ensure` (which validates a
/// recorded page before falling back to create-or-adopt) and `reconcile`
/// (which validates first via a broad title search, then this same single-id
/// path when that search sees nothing). `unchanged_slug` is the only axis
/// the two doors disagree on: `ensure` calls its steady state `"adopted"`,
/// `reconcile` calls it `"unchanged"`.
async fn apply_notion_decision(
    state: &ApiState,
    project: &store::projects::Project,
    notion: &dyn cloud::NotionService,
    decision: cloud::NotionRepairDecision,
    unchanged_slug: &'static str,
) -> ProjectOutcome {
    match decision {
        // Unreachable from either caller here (both always pass exactly one
        // snapshot), kept exhaustive rather than panicking on a future
        // caller that passes more.
        cloud::NotionRepairDecision::Missing => {
            ProjectOutcome::plain(project.code.clone(), "missing")
        }
        cloud::NotionRepairDecision::Duplicate { page_ids } => ProjectOutcome::with_detail(
            project.code.clone(),
            "duplicate",
            format!("{} pages carry this code", page_ids.len()),
        ),
        cloud::NotionRepairDecision::Conflict { observed_code, .. } => {
            ProjectOutcome::with_detail(project.code.clone(), "renamed", observed_code)
        }
        cloud::NotionRepairDecision::Unavailable { .. } => {
            ProjectOutcome::plain(project.code.clone(), "archived")
        }
        cloud::NotionRepairDecision::Unchanged { .. } => {
            ProjectOutcome::plain(project.code.clone(), unchanged_slug)
        }
        cloud::NotionRepairDecision::Repair { page_id, .. } => {
            match notion.update_private_page(&page_id, &project.code).await {
                Ok(page) => {
                    if !notion.is_expected_parent(&page) {
                        return ProjectOutcome::plain(project.code.clone(), "wrong_parent");
                    }
                    if store::projects::set_private_notion_page_url(
                        &state.application_store,
                        project.id,
                        &page.url,
                    )
                    .await
                    .is_err()
                    {
                        return ProjectOutcome::plain(project.code.clone(), "address_not_recorded");
                    }
                    ProjectOutcome::plain(project.code.clone(), "repaired")
                }
                Err(_) => ProjectOutcome::plain(project.code.clone(), "provider_unavailable"),
            }
        }
    }
}

/// `POST /app/api/integrations/notion/reconcile` — compare each selected
/// Project against the environment-selected Notion database and report the
/// repair decision, applying the one decision that is a repair.
pub(crate) async fn notion_reconcile_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(selector): Json<ProjectSelector>,
) -> Result<Response, ApiError> {
    let mut results = Vec::new();
    for project in targets(&state, &authed, &selector).await? {
        results.push(reconcile_one_page(&state, &project).await);
    }
    Ok(report(results))
}

async fn reconcile_one_page(
    state: &ApiState,
    project: &store::projects::Project,
) -> ProjectOutcome {
    let notion = match state
        .integration_providers
        .notion(&state.application_store, project.id)
        .await
    {
        Ok(notion) => notion,
        Err(error) => return unavailable(project, &error),
    };
    // The recorded address is the canonical one. Reconciliation never guesses
    // through a stale URL, so a Project with no recorded page is `not_recorded`
    // even when the provider holds one — adopting it is `ensure`'s decision,
    // not a repair.
    let Some(canonical) = project.private_notion_page_url.clone() else {
        return ProjectOutcome::plain(project.code.clone(), "not_recorded");
    };
    let Ok(pages) = notion.list_private_pages(&project.code).await else {
        return ProjectOutcome::plain(project.code.clone(), "provider_unavailable");
    };
    if pages.iter().any(|page| !notion.is_expected_parent(page)) {
        return ProjectOutcome::plain(project.code.clone(), "wrong_parent");
    }
    if !pages.is_empty() {
        let snapshots: Vec<cloud::NotionPageSnapshot> =
            pages.into_iter().map(to_snapshot).collect();
        let input = cloud::NotionProjectInput {
            project_code: project.code.clone(),
            canonical_url: canonical,
        };
        let decision = cloud::reconcile_notion_project(&input, &snapshots);
        return apply_notion_decision(state, project, notion.as_ref(), decision, "unchanged").await;
    }
    // A title search cannot see a page that was renamed away from its
    // recorded title or archived — Notion excludes archived pages from
    // `/search`, and a rename means the title no longer matches the query.
    // Before reporting the recorded page truly missing, look it up by its
    // own id directly.
    let Some(page_id) = cloud::notion_page_id_from_url(&canonical) else {
        return ProjectOutcome::plain(project.code.clone(), "missing");
    };
    match notion.get_page(&page_id).await {
        Ok(Some(page)) => {
            if !notion.is_expected_parent(&page) {
                return ProjectOutcome::plain(project.code.clone(), "wrong_parent");
            }
            let input = cloud::NotionProjectInput {
                project_code: project.code.clone(),
                canonical_url: canonical,
            };
            let snapshot = to_snapshot(page);
            let decision = cloud::reconcile_notion_project(&input, std::slice::from_ref(&snapshot));
            apply_notion_decision(state, project, notion.as_ref(), decision, "unchanged").await
        }
        Ok(None) => ProjectOutcome::plain(project.code.clone(), "missing"),
        Err(_) => ProjectOutcome::plain(project.code.clone(), "provider_unavailable"),
    }
}

/// The Project's channel name, from (environment, code) alone (ENG-1286):
/// `navigator-<code>` in production, `staging-<code>` on the simulated-matters
/// deployment. The same function names the channel the worker's lanes create,
/// so the two never provision two channels for one Project.
fn project_channel_name(code: &str) -> String {
    workflows::project_channel_name(cloud::labels_slack_from_staging_env(), code)
}

/// `POST /app/api/integrations/slack/ensure` — create or adopt one Project's
/// Firm-private channel and record its channel id and canonical URL.
///
/// The invite list is empty by construction. The adapter accepts only
/// provider-issued member ids, Navigator stores none, and it will not turn a
/// participation row or an email address into an invite — so the Firm's own
/// Slack membership governs who joins the channel it just made.
pub(crate) async fn slack_ensure_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(request): Json<SlackRequest>,
) -> Result<Response, ApiError> {
    let project = one_target(&state, &authed, &request.project_code).await?;
    Ok(report_one(ensure_one_channel(&state, &project).await))
}

async fn ensure_one_channel(
    state: &ApiState,
    project: &store::projects::Project,
) -> ProjectOutcome {
    let _guard =
        ensure_lock(workflows::IntegrationJob::slack(project.id, &project.code).job_id).await;
    let slack = match state
        .integration_providers
        .slack(&state.application_store, project.id)
        .await
    {
        Ok(slack) => slack,
        Err(error) => return unavailable(project, &error),
    };
    let channel_name = project_channel_name(&project.code);
    // A recorded channel is validated by its own id, never rediscovered by
    // name search: `conversations.list` excludes archived channels and a
    // rename means the name no longer matches the query, so treating "the
    // search found nothing" as "nothing was ever provisioned" would create a
    // duplicate beside a channel that still exists.
    if let Some(recorded_id) = project.internal_slack_channel_id.clone() {
        match slack.get_channel(&recorded_id).await {
            Ok(Some(channel)) if channel.is_archived => {
                return ProjectOutcome::plain(project.code.clone(), "archived")
            }
            Ok(Some(channel)) if !channel.is_private => {
                return ProjectOutcome::plain(project.code.clone(), "not_private")
            }
            Ok(Some(channel)) if channel.name != channel_name => {
                return repair_channel_name(state, project, slack.as_ref(), &channel, &channel_name)
                    .await
            }
            Ok(Some(channel)) => {
                let canonical_url = cloud::slack_channel_url(&channel.id);
                if project
                    .internal_slack_channel_url
                    .as_deref()
                    .is_some_and(|url| url != canonical_url)
                {
                    return ProjectOutcome::plain(project.code.clone(), "address_mismatch");
                }
                if store::projects::set_internal_slack_channel(
                    &state.application_store,
                    project.id,
                    &channel.id,
                    &canonical_url,
                )
                .await
                .is_err()
                {
                    return ProjectOutcome::plain(project.code.clone(), "address_not_recorded");
                }
                return ProjectOutcome::plain(project.code.clone(), "adopted");
            }
            Ok(None) => {
                return ProjectOutcome::plain(project.code.clone(), "recorded_resource_missing")
            }
            Err(_) => return ProjectOutcome::plain(project.code.clone(), "provider_unavailable"),
        }
    }
    let (channel, created) =
        match cloud::ensure_private_channel(slack.as_ref(), &channel_name, &[]).await {
            Ok(result) => result,
            // A name collision is reported as its own outcome — an operator
            // decision (something already holds this name) — never folded into
            // the generic provider-outage slug.
            Err(cloud::SlackError::NameTaken) => {
                return ProjectOutcome::plain(project.code.clone(), "conflict")
            }
            Err(cloud::SlackError::NotPrivate) => {
                return ProjectOutcome::plain(project.code.clone(), "not_private")
            }
            Err(_) => return ProjectOutcome::plain(project.code.clone(), "provider_unavailable"),
        };
    // Record the id and its canonical URL together so the two coordinates the
    // Project row carries for this channel never disagree.
    if store::projects::set_internal_slack_channel(
        &state.application_store,
        project.id,
        &channel.id,
        &cloud::slack_channel_url(&channel.id),
    )
    .await
    .is_err()
    {
        return ProjectOutcome::plain(project.code.clone(), "address_not_recorded");
    }
    ProjectOutcome::plain(
        project.code.clone(),
        if created { "created" } else { "adopted" },
    )
}

/// A recorded channel exists under another name — the pre-ENG-1286
/// `project-<code>` or a bare code, or a rename by hand. The reconcile renames
/// it back: Slack keeps the id, so the recorded coordinate and the channel's
/// history survive, and the former name is reported as the detail. A taken
/// canonical name is an operator decision, reported as `conflict`.
async fn repair_channel_name(
    state: &ApiState,
    project: &store::projects::Project,
    slack: &dyn cloud::SlackService,
    channel: &cloud::FirmSlackChannel,
    channel_name: &str,
) -> ProjectOutcome {
    let former = channel.name.clone();
    let channel = match slack.rename_channel(&channel.id, channel_name).await {
        Ok(channel) => channel,
        Err(cloud::SlackError::NameTaken) => {
            return ProjectOutcome::with_detail(project.code.clone(), "conflict", former)
        }
        Err(_) => return ProjectOutcome::plain(project.code.clone(), "provider_unavailable"),
    };
    if store::projects::set_internal_slack_channel(
        &state.application_store,
        project.id,
        &channel.id,
        &cloud::slack_channel_url(&channel.id),
    )
    .await
    .is_err()
    {
        return ProjectOutcome::plain(project.code.clone(), "address_not_recorded");
    }
    ProjectOutcome::with_detail(project.code.clone(), "repaired", former)
}

/// `POST /app/api/integrations/slack/notify` — post one closed-vocabulary,
/// mechanism-only notice to a Project's Firm-private channel.
///
/// An unrecognized event is a 400 before any provider call. The vocabulary is
/// closed precisely so this door cannot become a way to put arbitrary text —
/// a client name, a document title — into a channel.
pub(crate) async fn slack_notify_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(request): Json<SlackRequest>,
) -> Result<Response, ApiError> {
    let event = request
        .event
        .as_deref()
        .and_then(workflows::SlackNoticeEvent::parse)
        .ok_or_else(|| {
            ApiError::MalformedBody(format!(
                "event must be one of {}",
                workflows::SlackNoticeEvent::ALL
                    .map(workflows::SlackNoticeEvent::as_str)
                    .join(", ")
            ))
        })?;
    let project = one_target(&state, &authed, &request.project_code).await?;
    let slack = match state
        .integration_providers
        .slack(&state.application_store, project.id)
        .await
    {
        Ok(slack) => slack,
        Err(error) => return Ok(report_one(unavailable(&project, &error))),
    };
    // Notify never provisions. A missing channel is reported so an operator
    // runs `ensure` deliberately, rather than a notice quietly creating the
    // Firm-private channel it wanted to post into.
    let Ok(found) = slack
        .find_private_channel(&project_channel_name(&project.code))
        .await
    else {
        return Ok(report_one(ProjectOutcome::plain(
            project.code.clone(),
            "provider_unavailable",
        )));
    };
    let Some(channel) = found else {
        return Ok(report_one(ProjectOutcome::plain(
            project.code.clone(),
            "no_channel",
        )));
    };
    match workflows::post_slack_notice(slack.as_ref(), &channel.id, event).await {
        Ok(()) => Ok(report_one(ProjectOutcome::plain(
            project.code.clone(),
            "notified",
        ))),
        Err(_) => Ok(report_one(ProjectOutcome::plain(
            project.code.clone(),
            "provider_unavailable",
        ))),
    }
}

/// `{ "project_code": "…", "all": false, "dry_run": true }` for the Slack
/// lifecycle door.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlackLifecycleRequest {
    #[serde(default)]
    project_code: Option<String>,
    #[serde(default)]
    all: bool,
    #[serde(default)]
    dry_run: bool,
}

/// `POST /app/api/integrations/slack/lifecycle` — bring each selected
/// Project's recorded private channel in step with the Project's status: a
/// closed or archived Project's channel is archived, an open Project's
/// archived channel is unarchived. Archiving keeps the channel's history,
/// which is part of the matter's record; nothing here deletes a channel. The
/// channel is found by its recorded id, never by name, and a Project with no
/// recorded channel is reported, not provisioned. A dry run reports
/// `would_archive` or `would_unarchive` and changes nothing. Each change is
/// logged against the Project.
pub(crate) async fn slack_lifecycle_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(request): Json<SlackLifecycleRequest>,
) -> Result<Response, ApiError> {
    let selector = ProjectSelector {
        project_code: request.project_code,
        all: request.all,
    };
    let mut results = Vec::new();
    for project in targets(&state, &authed, &selector).await? {
        results.push(
            converge_project_channel(&state, authed.0.person_id, &project, request.dry_run).await,
        );
    }
    Ok(report(results))
}

/// Bring one Project's recorded channel in step with its status. The
/// lifecycle door runs it for each selected Project; the lifecycle move
/// (`POST /app/api/projects/{id}/lifecycle`) runs it right after the status
/// changes, so a close or a reopen does not wait for a sweep.
pub(crate) async fn converge_project_channel(
    state: &ApiState,
    actor_person_id: Option<uuid::Uuid>,
    project: &store::projects::Project,
    dry_run: bool,
) -> ProjectOutcome {
    let slack = match state
        .integration_providers
        .slack(&state.application_store, project.id)
        .await
    {
        Ok(slack) => slack,
        Err(error) => return unavailable(project, &error),
    };
    let open = project.status == "open";
    let Some(channel_id) = project.internal_slack_channel_id.clone() else {
        return ProjectOutcome::plain(project.code.clone(), "no_channel");
    };
    let _guard =
        ensure_lock(workflows::IntegrationJob::slack(project.id, &project.code).job_id).await;
    let outcome =
        match cloud::converge_recorded_channel(slack.as_ref(), &channel_id, open, dry_run).await {
            Ok(cloud::ChannelLifecycle::Archived) => "archived",
            Ok(cloud::ChannelLifecycle::Unarchived) => "unarchived",
            Ok(cloud::ChannelLifecycle::Unchanged) => "unchanged",
            Ok(cloud::ChannelLifecycle::WouldArchive) => "would_archive",
            Ok(cloud::ChannelLifecycle::WouldUnarchive) => "would_unarchive",
            Ok(cloud::ChannelLifecycle::Missing) => "recorded_resource_missing",
            Err(_) => "provider_unavailable",
        };
    match outcome {
        "archived" => {
            log_channel_change(state, actor_person_id, project, "slack_channel_archived").await;
        }
        "unarchived" => {
            log_channel_change(state, actor_person_id, project, "slack_channel_unarchived").await;
        }
        _ => {}
    }
    ProjectOutcome::plain(project.code.clone(), outcome)
}

/// Record one channel change on the Project's Relationship Log. A log write
/// that fails is reported in the server log and never undoes the change.
async fn log_channel_change(
    state: &ApiState,
    actor_person_id: Option<uuid::Uuid>,
    project: &store::projects::Project,
    action: &str,
) {
    let entry = store::relationship_logs::NewRelationshipLog {
        actor_person_id,
        subject_type: "project".to_string(),
        subject_id: project.id,
        action: action.to_string(),
        detail: format!("Project status {}", project.status),
        project_id: Some(project.id),
    };
    if let Err(error) = store::relationship_logs::record(&state.application_store, &entry).await {
        tracing::warn!(project_id = %project.id, %error, "slack channel change not logged");
    }
}

/// A refusal the CI step prints, so a lost notice never reads as delivered.
fn pull_request_refusal(status: StatusCode, error: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": error, "message": message })),
    )
        .into_response()
}

/// `POST /app/api/projects/{id}/events/pull-request` — a Project repository's
/// CI reports one pull-request run. The caller is the document-scoped CI
/// session minted for exactly this Project; any other session is refused with
/// 403. The first report of a pull request posts one message to the Project's
/// recorded private channel with the owning Firm's Slack bot; every later
/// report edits that message in place, so the channel holds one line per pull
/// request. The door never provisions the channel: a Project without one is
/// refused with 409 until `navigator project setup` creates it.
pub(crate) async fn pull_request_event_door(
    State(state): State<ApiState>,
    authed: crate::api::AuthedSession,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(event): Json<cloud::pull_request_notice::PullRequestEvent>,
) -> Result<Response, ApiError> {
    match &authed.0.scope {
        Some(crate::session::SessionScope::Document(scope)) if scope.project_id == id => {}
        _ => return Err(ApiError::Forbidden),
    }
    if let Err(message) = event.validate() {
        return Ok(crate::api::bad_request("invalid_event", message));
    }
    let project = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let slack = match state
        .integration_providers
        .slack(&state.application_store, id)
        .await
    {
        Ok(slack) => slack,
        Err(error) => {
            return Ok(pull_request_refusal(
                StatusCode::CONFLICT,
                "slack_not_connected",
                &format!("the pull request notice was not sent: {error}"),
            ))
        }
    };
    let Some(channel_id) = project.internal_slack_channel_id.clone() else {
        return Ok(pull_request_refusal(
            StatusCode::CONFLICT,
            "no_channel",
            "the Project has no private Slack channel yet; run `navigator project setup`",
        ));
    };
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| ApiError::Db("pull request notice storage is unavailable".into()))?;
    // Two runs of one pull request must not both post: the second waits and
    // edits what the first recorded.
    let _guard = ensure_lock(format!("pull-request:{id}:{}", event.number)).await;
    let recorded = store::postgres::pull_request_notices::find(&pg, id, event.number)
        .await
        .map_err(|_| ApiError::Db("pull request notice read failed".into()))?
        .filter(|notice| notice.channel_id == channel_id)
        .map(|notice| notice.message_ts);
    let Ok(published) = cloud::pull_request_notice::publish(
        slack.as_ref(),
        &channel_id,
        recorded.as_deref(),
        &event,
    )
    .await
    else {
        return Ok(pull_request_refusal(
            StatusCode::BAD_GATEWAY,
            "provider_unavailable",
            "Slack did not accept the pull request notice",
        ));
    };
    store::postgres::pull_request_notices::record(
        &pg,
        id,
        event.number,
        &store::postgres::pull_request_notices::PullRequestNotice {
            channel_id,
            message_ts: published.ts().to_string(),
        },
    )
    .await
    .map_err(|_| ApiError::Db("pull request notice write failed".into()))?;
    let outcome = match published {
        cloud::pull_request_notice::Published::Posted(_) => "posted",
        cloud::pull_request_notice::Published::Updated(_) => "updated",
    };
    Ok(Json(serde_json::json!({ "outcome": outcome })).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CalendarEventsRequest {
    project_code: String,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
}

/// List events on a synthetic Project Calendar on staging.
pub(crate) async fn calendar_events_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(request): Json<CalendarEventsRequest>,
) -> Result<Response, ApiError> {
    let environment = |key: &str| std::env::var(key).ok();
    if store::config::staging_from(environment) != Ok(true)
        || store::sample_matters_from(store::DeploymentEnvironment::Production, environment)
            != Ok(true)
        || !matches!(
            request.project_code.as_str(),
            store::seed::SAMPLE_LITIGATION_CODE
                | store::seed::SAMPLE_TRANSACTIONAL_CODE
                | store::seed::SAMPLE_ESTATE_CODE
        )
    {
        return Err(ApiError::NotFound);
    }
    if request.start >= request.end || request.end - request.start > TimeDelta::days(31) {
        return Err(ApiError::MalformedBody(
            "provide a positive calendar window of at most 31 days".to_owned(),
        ));
    }
    let project = one_target(&state, &authed, &request.project_code).await?;
    let stored_id = project.calendar_id.as_deref().ok_or(ApiError::NotFound)?;
    let unavailable = || (StatusCode::BAD_GATEWAY, "Project Calendar unavailable").into_response();
    let Ok(calendar) = state
        .integration_providers
        .calendar(&state.application_store, project.id)
        .await
    else {
        return Ok(unavailable());
    };
    match calendar
        .project_events(&project.code, stored_id, request.start, request.end)
        .await
    {
        Ok(Some(events)) => {
            Ok(([(header::CACHE_CONTROL, "no-store")], Json(events)).into_response())
        }
        Ok(None) => Err(ApiError::NotFound),
        Err(_) => Ok(unavailable()),
    }
}

/// `POST /app/api/integrations/calendar/ensure` — create or adopt one
/// Project's shared Google Calendar, record its id, and share it with the
/// firm-side participants. A closed matter's calendar is made read-only
/// instead. See [`store::project_calendar`].
pub(crate) async fn calendar_ensure_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(selector): Json<ProjectSelector>,
) -> Result<Response, ApiError> {
    let mut results = Vec::new();
    for project in targets(&state, &authed, &selector).await? {
        results.push(ensure_one_calendar(&state, &project).await);
    }
    Ok((StatusCode::OK, Json(IntegrationReport { results })).into_response())
}

/// `POST /app/api/integrations/contacts/sync` — put the people on each firm
/// member's open matters into that member's own Google Contacts, for every
/// firm-side participant on the selected Projects. See
/// [`store::contact_sync`].
pub(crate) async fn contacts_sync_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(selector): Json<ProjectSelector>,
) -> Result<Response, ApiError> {
    let mut results = Vec::new();
    for project in targets(&state, &authed, &selector).await? {
        results.push(sync_one_contacts(&state, &project).await);
    }
    Ok((StatusCode::OK, Json(IntegrationReport { results })).into_response())
}

async fn sync_one_contacts(state: &ApiState, project: &store::projects::Project) -> ProjectOutcome {
    let books = match state.integration_providers.contacts().await {
        Ok(books) => books,
        Err(error) => return unavailable(project, &error),
    };
    match store::contact_sync::sync_project_members(
        &state.application_store,
        project.id,
        books.as_ref(),
    )
    .await
    {
        Ok(report) => ProjectOutcome::with_detail(
            project.code.clone(),
            "synced",
            format!(
                "{} members: {} created, {} updated, {} removed",
                report.members, report.created, report.updated, report.removed
            ),
        ),
        Err(store::contact_sync::ContactSyncError::Contacts(_)) => {
            ProjectOutcome::plain(project.code.clone(), "provider_unavailable")
        }
        Err(store::contact_sync::ContactSyncError::NotVerified) => {
            ProjectOutcome::plain(project.code.clone(), "verification_failed")
        }
        Err(_) => ProjectOutcome::plain(project.code.clone(), "address_not_recorded"),
    }
}

async fn ensure_one_calendar(
    state: &ApiState,
    project: &store::projects::Project,
) -> ProjectOutcome {
    let calendar = match state
        .integration_providers
        .calendar(&state.application_store, project.id)
        .await
    {
        Ok(calendar) => calendar,
        Err(error) => return unavailable(project, &error),
    };
    let Ok(_guard) = calendar_lock(project.id).await else {
        return ProjectOutcome::plain(project.code.clone(), "provider_unavailable");
    };
    match store::project_calendar::ensure(&state.application_store, project.id, calendar.as_ref())
        .await
    {
        Ok(report) => ProjectOutcome::plain(project.code.clone(), report.outcome.slug()),
        Err(store::project_calendar::ProjectCalendarError::Calendar(
            cloud::CalendarError::AmbiguousSummary { count },
        )) => ProjectOutcome::with_detail(
            project.code.clone(),
            "duplicate",
            format!("{count} calendars carry this code"),
        ),
        Err(store::project_calendar::ProjectCalendarError::Calendar(_)) => {
            ProjectOutcome::plain(project.code.clone(), "provider_unavailable")
        }
        Err(_) => ProjectOutcome::plain(project.code.clone(), "address_not_recorded"),
    }
}

/// A provider client that could not be built. The unconfigured runtime
/// carries its detail — the missing deployment configuration and the step
/// that supplies it — the way a skipped Drive surface does.
fn unavailable(project: &store::projects::Project, error: &IntegrationError) -> ProjectOutcome {
    match error.detail() {
        Some(detail) => {
            ProjectOutcome::with_detail(project.code.clone(), error.slug(), detail.to_string())
        }
        None => ProjectOutcome::plain(project.code.clone(), error.slug()),
    }
}

/// `{ "project_code": "…", "all": false, "dry_run": true }` — the ensure
/// door's selector with a dry-run flag beside it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeadlineMirrorRequest {
    #[serde(default)]
    pub(crate) project_code: Option<String>,
    #[serde(default)]
    pub(crate) all: bool,
    #[serde(default)]
    pub(crate) dry_run: bool,
}

/// `POST /app/api/integrations/calendar/deadline-mirror` — reconcile one
/// Project's mirrored Google Calendar events to its Deadline rows, or every
/// visible matter's at once. The applied pass ensures the calendar exists
/// and is shared before mirroring; a dry run plans the same writes without
/// making them. See [`store::deadline_calendar`].
pub(crate) async fn calendar_deadline_mirror_door(
    State(state): State<ApiState>,
    authed: AdminSession,
    Json(request): Json<DeadlineMirrorRequest>,
) -> Result<Response, ApiError> {
    let selector = ProjectSelector {
        project_code: request.project_code.clone(),
        all: request.all,
    };
    let start = std::time::Instant::now();
    let selected = targets(&state, &authed, &selector).await;
    tracing::debug!(
        elapsed_ms = start.elapsed().as_secs_f64() * 1000.0,
        "Deadline mirror Project lookup"
    );
    let mut results = Vec::new();
    for project in selected? {
        let start = std::time::Instant::now();
        let result = mirror_one(&state, &project, request.dry_run).await;
        tracing::debug!(
            project_id = %project.id,
            elapsed_ms = start.elapsed().as_secs_f64() * 1000.0,
            outcome = result.outcome,
            "Deadline mirror reconciliation"
        );
        results.push(result);
    }
    Ok((StatusCode::OK, Json(IntegrationReport { results })).into_response())
}

pub(crate) async fn mirror_one(
    state: &ApiState,
    project: &store::projects::Project,
    dry_run: bool,
) -> ProjectOutcome {
    let calendar = match state
        .integration_providers
        .calendar(&state.application_store, project.id)
        .await
    {
        Ok(calendar) => calendar,
        Err(error) => return unavailable(project, &error),
    };
    let Ok(_guard) = calendar_lock(project.id).await else {
        return ProjectOutcome::plain(project.code.clone(), "provider_unavailable");
    };
    match store::deadline_calendar::reconcile(
        &state.application_store,
        calendar.as_ref(),
        project.id,
        state.canonical_base_url.as_deref(),
        dry_run,
    )
    .await
    {
        Ok(report) => mirror_outcome(project.code.clone(), &report),
        Err(store::deadline_calendar::MirrorError::Calendar(
            cloud::CalendarError::AmbiguousSummary { count },
        )) => ProjectOutcome::with_detail(
            project.code.clone(),
            "duplicate",
            format!("{count} calendars carry this code"),
        ),
        Err(store::deadline_calendar::MirrorError::Calendar(error)) => {
            let recorded = calendar_recorded(state, project.id).await;
            mirror_failure(project.code.clone(), recorded, &error)
        }
        Err(_) => ProjectOutcome {
            calendar_recorded: calendar_recorded(state, project.id).await,
            ..ProjectOutcome::plain(project.code.clone(), "provider_unavailable")
        },
    }
}

async fn calendar_recorded(state: &ApiState, project_id: uuid::Uuid) -> Option<bool> {
    let project = store::projects::find_by_id(&state.application_store, project_id)
        .await
        .ok()??;
    Some(project.calendar_id.is_some_and(|id| !id.trim().is_empty()))
}

/// A finished pass. Rows the provider refused are writes that did not
/// happen, so the pass reports `mirror_incomplete` with one line per row.
fn mirror_outcome(
    project_code: String,
    report: &store::deadline_calendar::MirrorReport,
) -> ProjectOutcome {
    use store::deadline_calendar::MirrorOutcome;

    let mut line = store::deadline_calendar::report_line(report);
    if report.dry_run && report.creates_calendar {
        line.push_str("; creates or adopts the Project calendar first");
    }
    let outcome = if report.failures.is_empty() {
        report.outcome.slug()
    } else {
        tracing::warn!(
            failures = report.failures.len(),
            "Deadline Calendar reconcile has pending events"
        );
        for failure in &report.failures {
            line.push_str("; ");
            line.push_str(failure);
        }
        "mirror_incomplete"
    };
    ProjectOutcome {
        calendar_recorded: match report.outcome {
            MirrorOutcome::Mirrored => Some(!report.creates_calendar),
            MirrorOutcome::RecordedResourceMissing | MirrorOutcome::SkippedClosed => None,
        },
        pending_changes: (report.dry_run && outcome == "mirrored")
            .then_some(report.upserted.len() + report.deleted.len()),
        ..ProjectOutcome::with_detail(project_code, outcome, line)
    }
}

/// The outcome a provider refusal reports. With no recorded calendar the
/// Project's calendar was never provisioned, and that is what the lawyer
/// needs to hear; the provider's class rides in the detail.
fn mirror_failure(
    project_code: String,
    recorded: Option<bool>,
    error: &cloud::CalendarError,
) -> ProjectOutcome {
    let cause = calendar_failure_slug(error);
    let (reason, action) = calendar_provider_failure(error);
    ProjectOutcome {
        calendar_recorded: recorded,
        provider_reason: Some(reason),
        provider_action: action,
        ..if recorded == Some(false) {
            ProjectOutcome::with_detail(
                project_code,
                "calendar_unprovisioned",
                format!("no calendar recorded; creating or adopting it failed with {cause}"),
            )
        } else {
            ProjectOutcome::plain(project_code, cause)
        }
    }
}

fn calendar_provider_failure(error: &cloud::CalendarError) -> (&'static str, Option<&'static str>) {
    use cloud::CalendarError;
    match error {
        CalendarError::MissingConfig(_) => ("missing_configuration", None),
        CalendarError::Authentication => ("authentication_failed", Some("authenticating")),
        CalendarError::Api { action, status } => (
            match status {
                401 => "credential_rejected",
                403 => "provider_forbidden",
                429 => "rate_limited",
                500..=599 => "provider_error",
                _ => "request_refused",
            },
            Some(*action),
        ),
        CalendarError::Request { action, source } => (
            if source.is_timeout() {
                "request_timeout"
            } else {
                "transport_error"
            },
            Some(*action),
        ),
        CalendarError::Response { action, .. } => ("invalid_response", Some(*action)),
        _ => ("unverified_provider_state", None),
    }
}

/// Sort a provider refusal into the operator's next step: an unconfigured
/// runtime, a delegation the Workspace refuses, or a failure a retry clears.
const fn calendar_failure_slug(error: &cloud::CalendarError) -> &'static str {
    match error {
        cloud::CalendarError::MissingConfig(_) => "runtime_not_configured",
        cloud::CalendarError::Authentication
        | cloud::CalendarError::Api {
            status: 401 | 403, ..
        } => "provider_unauthorized",
        _ => "provider_unavailable",
    }
}

fn to_snapshot(page: cloud::NotionPage) -> cloud::NotionPageSnapshot {
    cloud::NotionPageSnapshot {
        accessible: !page.archived,
        id: page.id,
        url: page.url,
        project_code: page.project_code,
    }
}

fn report(results: Vec<ProjectOutcome>) -> Response {
    (StatusCode::OK, Json(IntegrationReport { results })).into_response()
}

fn report_one(outcome: ProjectOutcome) -> Response {
    report(vec![outcome])
}

#[cfg(test)]
mod tests {
    use super::{calendar_failure_slug, calendar_provider_failure, mirror_failure, mirror_outcome};
    use store::deadline_calendar::{MirrorOutcome, MirrorReport};

    #[test]
    fn a_provider_refusal_names_its_class() {
        use cloud::CalendarError;
        assert_eq!(
            calendar_failure_slug(&CalendarError::Authentication),
            "provider_unauthorized"
        );
        assert_eq!(
            calendar_failure_slug(&CalendarError::Api {
                action: "finding a calendar",
                status: 403
            }),
            "provider_unauthorized"
        );
        assert_eq!(
            calendar_failure_slug(&CalendarError::Api {
                action: "finding a calendar",
                status: 503
            }),
            "provider_unavailable"
        );
        assert_eq!(
            calendar_failure_slug(&CalendarError::MissingConfig("owner".into())),
            "runtime_not_configured"
        );
        for (status, reason) in [
            (401, "credential_rejected"),
            (403, "provider_forbidden"),
            (429, "rate_limited"),
            (503, "provider_error"),
        ] {
            let error = CalendarError::Api {
                action: "finding a calendar",
                status,
            };
            assert_eq!(
                calendar_provider_failure(&error),
                (reason, Some("finding a calendar"))
            );
        }
    }

    #[test]
    fn a_failure_without_a_recorded_calendar_is_unprovisioned() {
        let error = cloud::CalendarError::Api {
            action: "creating a calendar",
            status: 403,
        };
        let unprovisioned = mirror_failure("acme".into(), Some(false), &error);
        assert_eq!(unprovisioned.outcome, "calendar_unprovisioned");
        assert_eq!(
            unprovisioned.detail.as_deref(),
            Some("no calendar recorded; creating or adopting it failed with provider_unauthorized")
        );
        assert_eq!(unprovisioned.provider_reason, Some("provider_forbidden"));
        assert_eq!(unprovisioned.provider_action, Some("creating a calendar"));
        let recorded = mirror_failure("acme".into(), Some(true), &error);
        assert_eq!(recorded.outcome, "provider_unauthorized");
        assert_eq!(recorded.detail, None);
    }

    #[test]
    fn refused_rows_make_the_pass_incomplete() {
        let mut report = MirrorReport {
            outcome: MirrorOutcome::Mirrored,
            in_sync: 2,
            ..MirrorReport::default()
        };
        assert_eq!(mirror_outcome("acme".into(), &report).outcome, "mirrored");
        report
            .failures
            .push("deadline 1: Calendar API returned 500 while inserting an event".into());
        let incomplete = mirror_outcome("acme".into(), &report);
        assert_eq!(incomplete.outcome, "mirror_incomplete");
        assert_eq!(
            incomplete.detail.as_deref(),
            Some(
                "mirrored upserted=0 in_sync=2 deleted=0 failed=1; \
                 deadline 1: Calendar API returned 500 while inserting an event"
            )
        );
    }

    #[test]
    fn a_calendarless_plan_says_it_creates_the_calendar() {
        let report = MirrorReport {
            dry_run: true,
            creates_calendar: true,
            ..MirrorReport::default()
        };
        let outcome = mirror_outcome("acme".into(), &report);
        assert_eq!(outcome.calendar_recorded, Some(false));
        assert_eq!(outcome.pending_changes, Some(0));
        assert_eq!(
            outcome.detail.as_deref(),
            Some("mirrored upserted=0 in_sync=0 deleted=0 (dry run); creates or adopts the Project calendar first")
        );
    }
}

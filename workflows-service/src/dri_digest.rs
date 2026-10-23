//! The `DriDigest` Restate workflow — a nightly Slack notice listing every
//! project with its lawyer DRI and client DRI.
//!
//! Two durable steps, journaled independently — a notify retry must never
//! re-run the query, and a query retry must never re-post the notice:
//!
//! 1. `ctx.run("query")` — read every project's [`store::projects::dri_digest`]
//!    summary: both DRI sides, so an unassigned side on either is visible
//!    from the notice itself rather than requiring a separate admin-tier
//!    directory read.
//! 2. `ctx.run("notify")` — render the summary as mrkdwn and post it to firm
//!    ops through the worker's [`Notifier`]. Posts to the notifier directly,
//!    like `Archives`, rather than through the
//!    `SlackOpsDelivery` code-fence adapter: the digest names people and
//!    projects as a bulleted list, not a fixed-width table, so it needs no
//!    fencing.
//!
//! Persistent staging does not rewrite the digest header: the Slack adapter
//! already appends a final `from Staging` line to every real post. A
//! deployment that does not disclose simulated matters gets two further
//! steps a simulated run never runs, because the count they post is a
//! real-matter-only signal:
//!
//! 3. `ctx.run("counts")` — [`store::projects::matter_open_pitch_counts`]:
//!    how many of the firm's own `"neon"`-brand matters are open, and how
//!    many of those are still pitches.
//! 4. `ctx.run("notify_counts")` — post those counts as a second, separate
//!    Slack message, journaled independently from steps 1-2 so a retry of
//!    either pair never re-runs the other.
//!
//! **Internal operations signal**, not client correspondence: it names
//! matters and the firm's own people by their already-internal DRI markers,
//! posted only to the firm ops Slack channel — never emailed to a client or
//! matter contact.
//!
//! The `dri-digest-trigger` `CronJob` fires nightly at 01:11 UTC, keyed on
//! the UTC run date so a same-day double-fire is a no-op.

use std::sync::Arc;

use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::projects::ProjectDriSummary;
use store::surreal::SurrealDb;
pub use workflows::dri_digest::dri_digest_message;
use workflows::Notifier;

/// Request body for `DriDigest::run`. Empty — the trigger only starts the
/// workflow — but kept as a struct so fields can be threaded later without
/// changing the handler signature.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {}

/// Invocation output: how many projects the digest covered.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DriDigestReport {
    pub projects: usize,
}

/// The nightly open-matters/pitches follow-up post, or `None` when this
/// deployment discloses simulated matters — that count is a real-matter-only
/// signal, so a staging run posts nothing here rather than a number nobody
/// should read as real.
///
/// Pure and exposed so the simulated/production branch is unit-tested
/// without a workflow context, the same seam [`dri_digest_message`] uses for
/// its own header text.
#[must_use]
pub fn open_matters_followup(simulated: bool, open: usize, pitches: usize) -> Option<String> {
    if simulated {
        return None;
    }
    Some(format!("Total open matters: {open}, pitches: {pitches}"))
}

/// Service registered with the Restate endpoint. Holds a `SurrealDB` clone (the
/// same connection the worker opened at boot, same shape as
/// `ReconcileInvoicesService`), the worker-side Slack [`Notifier`], and
/// whether this deployment discloses simulated matters
/// (`store::sample_matters`) — resolved once at boot in
/// `workflows-service/src/main.rs`, not re-read from the environment on
/// every run.
#[derive(Clone)]
pub struct DriDigestService {
    surreal: SurrealDb,
    notifier: Arc<dyn Notifier>,
    simulated: bool,
}

/// Build the opt-in DBOS worker context after its store connection is ready.
/// A disabled schedule does not validate delivery configuration.
pub fn dbos_context(
    surreal: Option<SurrealDb>,
    notifier: Arc<dyn Notifier>,
    simulated: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<workflows::dbos_engine::dri_digest::Context>, anyhow::Error> {
    if !workflows::dbos_engine::dri_digest::enabled(&lookup)? {
        return Ok(None);
    }
    let surreal = surreal.ok_or_else(|| anyhow::anyhow!("DriDigest database not configured"))?;
    workflows::dbos_engine::dri_digest::Context::from_lookup(surreal, notifier, simulated, lookup)
        .map(Some)
        .map_err(Into::into)
}

impl DriDigestService {
    #[must_use]
    pub fn new(surreal: SurrealDb, notifier: Arc<dyn Notifier>, simulated: bool) -> Self {
        Self {
            surreal,
            notifier,
            simulated,
        }
    }
}

#[cfg(test)]
mod dbos_context_tests {
    use super::dbos_context;
    use workflows::CapturingNotifier;

    #[tokio::test]
    async fn worker_context_is_gated_and_requires_delivery_configuration() {
        let disabled = dbos_context(
            None,
            std::sync::Arc::new(CapturingNotifier::new()),
            false,
            |_| None,
        )
        .expect("disabled schedule has no database or webhook requirement");
        assert!(disabled.is_none());

        let invalid = dbos_context(
            None,
            std::sync::Arc::new(CapturingNotifier::new()),
            false,
            |key| {
                (key == workflows::dbos_engine::dri_digest::ENABLED_ENV)
                    .then(|| "sometimes".to_owned())
            },
        );
        assert!(invalid.is_err(), "invalid opt-in fails closed");

        let surreal = store::test_support::mem_surreal().await;
        let enabled = |key: &str| match key {
            workflows::dbos_engine::dri_digest::ENABLED_ENV => Some("true".to_owned()),
            _ => None,
        };
        let missing_database = dbos_context(
            None,
            std::sync::Arc::new(CapturingNotifier::new()),
            false,
            |key| match key {
                workflows::dbos_engine::dri_digest::ENABLED_ENV => Some("true".to_owned()),
                "SLACK_WEBHOOK_URL" => Some("https://example.test/hook".to_owned()),
                _ => None,
            },
        );
        assert!(
            missing_database.is_err(),
            "enabled delivery requires its store"
        );

        let missing_webhook = dbos_context(
            Some(surreal.clone()),
            std::sync::Arc::new(CapturingNotifier::new()),
            false,
            enabled,
        );
        assert!(missing_webhook.is_err(), "enabled delivery fails closed");

        let configured = |key: &str| match key {
            workflows::dbos_engine::dri_digest::ENABLED_ENV => Some("true".to_owned()),
            "SLACK_WEBHOOK_URL" => Some("https://example.test/hook".to_owned()),
            _ => None,
        };
        let context = dbos_context(
            Some(surreal),
            std::sync::Arc::new(CapturingNotifier::new()),
            true,
            configured,
        )
        .expect("valid delivery configuration")
        .expect("enabled schedule has a context");
        drop(context);
    }
}

#[restate_sdk::workflow(name = "DriDigest")]
impl DriDigestService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        _req: Json<RunRequest>,
    ) -> Result<Json<DriDigestReport>, HandlerError> {
        // Step 1 — query every project's DRIs. A database error surfaces as a
        // retryable `HandlerError` so Restate replays just this step.
        let surreal = self.surreal.clone();
        let projects: Vec<ProjectDriSummary> = ctx
            .run(move || async move { Ok(Json(store::projects::dri_digest(&surreal).await?)) })
            .name("query")
            .await?
            .into_inner();

        // Step 2 — render + post, journaled separately so a query retry never
        // re-posts and a notify retry never re-reads the database.
        let message = dri_digest_message(&projects, self.simulated);
        let notifier = Arc::clone(&self.notifier);
        ctx.run(move || async move { notifier.notify(message).await.map_err(HandlerError::from) })
            .name("notify")
            .await?;

        // Steps 3-4 — a real deployment's open-matters/pitches follow-up.
        // Never runs for a simulated-matters deployment: that count is a
        // real-matter-only signal, so there is nothing to query or post.
        if !self.simulated {
            let surreal = self.surreal.clone();
            let (open, pitches) = ctx
                .run(move || async move {
                    store::projects::matter_open_pitch_counts(&surreal)
                        .await
                        .map(Json)
                        .map_err(|error| {
                            HandlerError::from(TerminalError::new(format!("counts: {error}")))
                        })
                })
                .name("counts")
                .await?
                .into_inner();

            // `simulated` is `false` in this branch, so this is always `Some`;
            // the shared helper still owns the decision so its own unit tests
            // are the single place that predicate is pinned.
            if let Some(followup) = open_matters_followup(self.simulated, open, pitches) {
                let notifier = Arc::clone(&self.notifier);
                ctx.run(move || async move {
                    notifier.notify(followup).await.map_err(HandlerError::from)
                })
                .name("notify_counts")
                .await?;
            }
        }

        Ok(Json(DriDigestReport {
            projects: projects.len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{dri_digest_message, open_matters_followup, ProjectDriSummary};

    fn project(
        code: &str,
        status: &str,
        lawyer_dris: &[&str],
        client_dris: &[&str],
    ) -> ProjectDriSummary {
        ProjectDriSummary {
            code: code.into(),
            name: code.into(),
            status: status.into(),
            lawyer_dris: lawyer_dris.iter().map(ToString::to_string).collect(),
            client_dris: client_dris.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn digest_names_both_dri_sides_per_project() {
        let msg = dri_digest_message(
            &[project(
                "sample-litigation",
                "open",
                &["Jane Roe"],
                &["Cruller Client"],
            )],
            false,
        );
        assert!(msg.starts_with("*Project DRI digest — 1 projects*"));
        assert!(
            msg.contains(
                "• *sample-litigation* (open) — Lawyer DRI: Jane Roe; Client DRI: Cruller Client"
            ),
            "{msg}"
        );
    }

    #[test]
    fn an_unassigned_side_reads_as_none_rather_than_blank() {
        let msg = dri_digest_message(&[project("sample-estate", "open", &[], &[])], false);
        assert!(
            msg.contains("Lawyer DRI: none; Client DRI: none"),
            "an empty side must say so: {msg}"
        );
    }

    #[test]
    fn multiple_dris_on_one_side_are_comma_joined() {
        let msg = dri_digest_message(
            &[project(
                "sample-transactional",
                "open",
                &["Jane Roe", "John Doe"],
                &[],
            )],
            false,
        );
        assert!(msg.contains("Lawyer DRI: Jane Roe, John Doe"), "{msg}");
    }

    #[test]
    fn header_counts_every_project_and_body_lists_each_one() {
        let msg = dri_digest_message(
            &[
                project("a", "open", &["Lawyer A"], &[]),
                project("b", "closed", &[], &["Client B"]),
            ],
            false,
        );
        assert!(msg.starts_with("*Project DRI digest — 2 projects*"));
        assert!(msg.contains("• *a* (open)"));
        assert!(msg.contains("• *b* (closed)"));
    }

    #[test]
    fn no_projects_still_renders_a_clean_header() {
        let msg = dri_digest_message(&[], false);
        assert_eq!(msg, "*Project DRI digest — 0 projects*");
    }

    #[test]
    fn a_simulated_run_does_not_rewrite_the_digest_header() {
        assert_eq!(
            dri_digest_message(&[], true),
            dri_digest_message(&[], false)
        );
        assert_eq!(
            dri_digest_message(&[], true),
            "*Project DRI digest — 0 projects*"
        );
    }

    /// A production run's header carries no staging wording; simulated
    /// copy matches it because the adapter owns the staging mark.
    #[test]
    fn a_non_simulated_run_carries_no_staging_suffix() {
        let msg = dri_digest_message(&[], false);
        assert!(
            !msg.contains("staging"),
            "a real production run must not disclose a staging account: {msg}"
        );
    }

    /// The whole point of the follow-up: a simulated deployment's open/pitch
    /// count is not a real-matter signal, so it must post nothing.
    #[test]
    fn a_simulated_deployment_posts_no_open_matters_followup() {
        assert_eq!(open_matters_followup(true, 3, 1), None);
    }

    /// A real deployment always posts the follow-up, with both counts named.
    #[test]
    fn a_real_deployment_posts_the_open_matters_followup() {
        assert_eq!(
            open_matters_followup(false, 3, 1),
            Some("Total open matters: 3, pitches: 1".to_string())
        );
    }

    /// Zero open matters (and so zero pitches) still posts — a quiet
    /// portfolio is still a real answer, not the simulated no-op.
    #[test]
    fn a_real_deployment_with_no_open_matters_still_posts_zero_counts() {
        assert_eq!(
            open_matters_followup(false, 0, 0),
            Some("Total open matters: 0, pitches: 0".to_string())
        );
    }
}

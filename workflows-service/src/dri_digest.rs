//! The `DriDigest` Restate workflow — a nightly Slack notice listing every
//! open project with its lawyer DRI and client DRI.
//!
//! Two durable steps, journaled independently — a notify retry must never
//! re-run the query, and a query retry must never re-post the notice:
//!
//! 1. `ctx.run("query")` — read each open project's [`store::projects::dri_digest`]
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
//! 3. `ctx.run("counts")` — [`workflows::dri_digest::slack_firm_open_pitch_counts`]:
//!    per Brand the deployment's Slack Firm owns, how many matters are open
//!    and how many of those are still pitches.
//! 4. `ctx.run("notify_counts")` — post the total and each Brand's line as a second, separate
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

#[cfg(feature = "restate-compatibility")]
use std::sync::Arc;

#[cfg(feature = "restate-compatibility")]
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
#[cfg(any(test, feature = "restate-compatibility"))]
use store::projects::ProjectDriSummary;
use store::surreal::SurrealDb;
pub use workflows::dri_digest::dri_digest_message;
#[cfg(feature = "restate-compatibility")]
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

/// The nightly open-matters/pitches follow-up post — the total, then each
/// Brand's line — or `None` when this
/// deployment discloses simulated matters — that count is a real-matter-only
/// signal, so a staging run posts nothing here rather than a number nobody
/// should read as real.
///
/// Pure and exposed so the simulated/production branch is unit-tested
/// without a workflow context, the same seam [`dri_digest_message`] uses for
/// its own header text.
#[must_use]
pub fn open_matters_followup(
    simulated: bool,
    brands: &[store::projects::BrandOpenPitch],
) -> Option<String> {
    (!simulated).then(|| workflows::dri_digest::open_matters_message(brands))
}

/// Service registered with the Restate endpoint. Holds a `SurrealDB` clone (the
/// same connection the worker opened at boot, same shape as
/// `ReconcileInvoicesService`), the worker-side Slack [`Notifier`], and
/// whether this deployment discloses simulated matters
/// (`store::sample_matters`) — resolved once at boot in
/// `workflows-service/src/host.rs`, not re-read from the environment on
/// every run.
#[derive(Clone)]
#[cfg(feature = "restate-compatibility")]
pub struct DriDigestService {
    surreal: SurrealDb,
    notifier: Arc<dyn Notifier>,
    simulated: bool,
}

/// Build the opt-in DBOS worker context after its store connection is ready.
/// A disabled schedule does not validate delivery configuration.
pub fn dbos_context(
    surreal: Option<SurrealDb>,
    slack: std::sync::Arc<dyn workflows::SlackBot>,
    simulated: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<workflows::dbos_engine::dri_digest::Context>, anyhow::Error> {
    if !workflows::dbos_engine::dri_digest::enabled(&lookup)? {
        return Ok(None);
    }
    let surreal = surreal.ok_or_else(|| anyhow::anyhow!("DriDigest database not configured"))?;
    workflows::dbos_engine::dri_digest::Context::from_lookup(surreal, slack, simulated, lookup)
        .map(Some)
        .map_err(Into::into)
}

#[cfg(feature = "restate-compatibility")]
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

    #[test]
    fn worker_context_is_gated_and_requires_delivery_configuration() {
        let bot = || std::sync::Arc::new(workflows::CapturingSlackBot::new());
        let enabled = workflows::dbos_engine::dri_digest::ENABLED_ENV;
        let disabled = dbos_context(None, bot(), false, |_| None)
            .expect("disabled schedule has no database or channel requirement");
        assert!(disabled.is_none());

        let invalid = dbos_context(None, bot(), false, |key| {
            (key == enabled).then(|| "sometimes".to_owned())
        });
        assert!(invalid.is_err(), "invalid opt-in fails closed");

        let configured = |key: &str| match key {
            key if key == enabled => Some("true".to_owned()),
            "SLACK_GENERAL_CHANNEL_ID" => Some("C0GENERAL".to_owned()),
            _ => None,
        };
        let surreal = store::surreal::SurrealDb::uninitialized;
        assert!(
            dbos_context(None, bot(), false, configured).is_err(),
            "enabled delivery requires its store"
        );
        assert!(
            dbos_context(Some(surreal()), bot(), false, |key| {
                (key == enabled).then(|| "true".to_owned())
            })
            .is_err(),
            "enabled delivery requires its channel"
        );
        assert!(dbos_context(Some(surreal()), bot(), true, configured)
            .unwrap()
            .is_some());
    }
}

#[cfg(feature = "restate-compatibility")]
#[restate_sdk::workflow(name = "DriDigest")]
impl DriDigestService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        _req: Json<RunRequest>,
    ) -> Result<Json<DriDigestReport>, HandlerError> {
        // Step 1 — query each open project's DRIs. A database error surfaces as a
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
            let brands = ctx
                .run(move || async move {
                    workflows::dri_digest::slack_firm_open_pitch_counts(
                        &surreal,
                        workflows::firm_slack::DeploymentSlackFirm::DefaultBrand,
                    )
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
            if let Some(followup) = open_matters_followup(self.simulated, &brands) {
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
    fn header_counts_the_projects_listed_in_the_body() {
        let msg = dri_digest_message(
            &[
                project("a", "open", &["Lawyer A"], &[]),
                project("b", "open", &[], &["Client B"]),
            ],
            false,
        );
        assert!(msg.starts_with("*Project DRI digest — 2 projects*"));
        assert!(msg.contains("• *a* (open)"));
        assert!(msg.contains("• *b* (open)"));
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

    /// A simulated deployment's open/pitch count is not a real-matter
    /// signal, so it posts nothing; a real one always posts, zero or not.
    #[test]
    fn only_a_real_deployment_posts_the_open_matters_followup() {
        let brands = [store::projects::BrandOpenPitch {
            brand: "Neon Law".into(),
            open: 3,
            pitches: 1,
        }];
        assert_eq!(open_matters_followup(true, &brands), None);
        assert_eq!(
            open_matters_followup(false, &brands).as_deref(),
            Some("Total open matters: 3, pitches: 1\n• Neon Law: 3 open, pitches: 1")
        );
        assert_eq!(
            open_matters_followup(false, &[]).as_deref(),
            Some("Total open matters: 0, pitches: 0")
        );
    }
}

//! The `GeneralNag` Restate workflow — the daily jab at `#general`.
//!
//! Two journaled posts, so a retry of one never re-runs the other:
//!
//! 1. `ctx.run("notify")` — post [`nag_message`] through the worker's Slack
//!    Web API bot to `SLACK_GENERAL_CHANNEL_ID`. Persistent staging does not
//!    rewrite this slogan: the Slack adapter already appends a final
//!    `from Staging` line to every real post.
//! 2. `ctx.run("counts")` / `ctx.run("notify_counts")` — a real deployment's
//!    open-matters/pitches follow-up, posted as a **second** Slack message.
//!    Never runs for a simulated-matters deployment: that count is a
//!    real-matter-only signal, so there is nothing to query or post. The
//!    copy is [`crate::dri_digest::open_matters_followup`].
//!
//! **Internal operations signal**, not client correspondence: the jab is a
//! static slogan, and the follow-up counts the Slack Firm's matters per Brand
//! with no matter names. Posted only to `#general`.
//!
//! The `general-nag` `CronJob` fires nightly at 01:11 UTC, keyed on the UTC
//! run date so a same-day double-fire is a no-op.

#[cfg(feature = "restate-compatibility")]
use std::sync::Arc;

#[cfg(feature = "restate-compatibility")]
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
#[cfg(feature = "restate-compatibility")]
use store::surreal::SurrealDb;
#[cfg(feature = "restate-compatibility")]
use workflows::SlackBot;

#[cfg(feature = "restate-compatibility")]
use crate::dri_digest::open_matters_followup;

/// Request body for `GeneralNag::run`. Empty — the trigger only starts the
/// workflow — but kept as a struct so fields can be threaded later without
/// changing the handler signature.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {}

/// Invocation output: whether the run posted the open-matters follow-up.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GeneralNagReport {
    pub posted_followup: bool,
}

/// The daily `#general` jab. Staging disclosure is the Slack adapter's
/// last-line `from Staging` mark, not a rewrite of this slogan.
#[must_use]
pub fn nag_message(_simulated: bool) -> String {
    "Nobody Cares, Work Harder".to_string()
}

/// Service registered with the Restate endpoint. Holds a `SurrealDB` clone
/// (the same connection the worker opened at boot), the target-aware Slack
/// bot, `#general`'s channel id, and whether this deployment discloses
/// simulated matters — resolved once at boot in `workflows-service/src/host.rs`.
#[derive(Clone)]
#[cfg(feature = "restate-compatibility")]
pub struct GeneralNagService {
    surreal: SurrealDb,
    slack: Arc<dyn SlackBot>,
    channel_id: String,
    simulated: bool,
}

#[cfg(feature = "restate-compatibility")]
impl GeneralNagService {
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        slack: Arc<dyn SlackBot>,
        channel_id: String,
        simulated: bool,
    ) -> Self {
        Self {
            surreal,
            slack,
            channel_id,
            simulated,
        }
    }
}

#[cfg(feature = "restate-compatibility")]
#[restate_sdk::workflow(name = "GeneralNag")]
impl GeneralNagService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        _req: Json<RunRequest>,
    ) -> Result<Json<GeneralNagReport>, HandlerError> {
        if self.channel_id.is_empty() {
            return Err(HandlerError::from(TerminalError::new(
                "SLACK_GENERAL_CHANNEL_ID must be set (the #general channel ID)",
            )));
        }

        let channel_id = self.channel_id.clone();
        let message = nag_message(self.simulated);
        let slack = Arc::clone(&self.slack);
        ctx.run(move || async move {
            slack
                .post_message(&channel_id, &message)
                .await
                .map_err(HandlerError::from)
        })
        .name("notify")
        .await?;

        let mut posted_followup = false;
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

            if let Some(followup) = open_matters_followup(self.simulated, &brands) {
                let channel_id = self.channel_id.clone();
                let slack = Arc::clone(&self.slack);
                ctx.run(move || async move {
                    slack
                        .post_message(&channel_id, &followup)
                        .await
                        .map_err(HandlerError::from)
                })
                .name("notify_counts")
                .await?;
                posted_followup = true;
            }
        }

        Ok(Json(GeneralNagReport { posted_followup }))
    }
}

#[cfg(test)]
mod tests {
    use super::nag_message;

    #[test]
    fn simulated_and_production_nags_share_one_slogan() {
        assert_eq!(nag_message(true), nag_message(false));
        assert_eq!(nag_message(false), "Nobody Cares, Work Harder");
        assert!(
            !nag_message(true).contains("staging"),
            "the slogan must not carry a staging parenthetical; the adapter marks staging"
        );
    }
}

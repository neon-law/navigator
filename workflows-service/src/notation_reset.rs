//! The `NotationReset` Restate workflow: the daily reset that deletes every
//! Notation draft on a deployment and says so in Slack.
//!
//! Keyed on the UTC date, so a second fire on the same day attaches to the
//! first; the Cron schedules console starts it on demand under a
//! `manual-<uuid>` key. The draft predicate is [`store::notation_reset`]'s
//! and is never restated here: this workflow discovers ids, deletes each one
//! in its own journaled step, and reports counts.
//!
//! Every run posts exactly one message to the deployment's internal
//! messages channel — `#messages` on a real deployment, `#staging-messages`
//! on a simulated-matters one — including a run that deleted nothing and a
//! run that failed before it could delete anything. A missing post therefore
//! means the job did not run. The post names the environment, the counts,
//! the duration, and the ids that failed to delete; never a client, a
//! matter, or a Project code.

use crate::workflow_step_error::WorkflowStepError;
#[cfg(test)]
use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;

pub use workflows::notation_reset::{
    environment_label, render_failure, render_report, run_key, DraftRoster, NotationResetReport,
};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {}

#[cfg(test)]
fn parse_clock(value: &str) -> Result<DateTime<Utc>, WorkflowStepError> {
    DateTime::parse_from_rfc3339(value)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| WorkflowStepError::terminal("the journaled clock is not RFC 3339"))
}

#[cfg(test)]
fn validate_configuration(channel_id: &str, slack_enabled: bool) -> Result<(), WorkflowStepError> {
    if channel_id.is_empty() {
        return Err(WorkflowStepError::terminal(
            "messages channel id for this deployment is not configured",
        ));
    }
    if !slack_enabled {
        return Err(WorkflowStepError::terminal(
            "the deployment's Slack bot must be configured for the notation reset",
        ));
    }
    Ok(())
}

/// Every draft id on the deployment, Notations and stored previews.
pub async fn discover_drafts(
    application_store: &ApplicationStore,
) -> Result<DraftRoster, WorkflowStepError> {
    workflows::notation_reset::discover_drafts(application_store)
        .await
        .map_err(|error| WorkflowStepError::terminal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_requires_a_channel_and_an_enabled_bot() {
        assert!(validate_configuration("C-MESSAGES", true).is_ok());
        assert!(validate_configuration("", true).is_err());
        assert!(validate_configuration("C-MESSAGES", false).is_err());
    }

    #[test]
    fn the_journaled_clock_must_be_rfc_3339() {
        let at = parse_clock("2026-10-21T04:20:00+02:00").expect("an RFC 3339 instant");
        assert_eq!(at.to_rfc3339(), "2026-10-21T02:20:00+00:00");
        let error = parse_clock("yesterday").expect_err("not RFC 3339");
        assert!(
            format!("{error:?}").contains("the journaled clock is not RFC 3339"),
            "{error:?}"
        );
    }

    #[test]
    fn configuration_failures_name_what_is_missing() {
        let channel = validate_configuration("", true).unwrap_err();
        assert!(format!("{channel:?}").contains("messages channel id"));
        let bot = validate_configuration("C-MESSAGES", false).unwrap_err();
        assert!(format!("{bot:?}").contains("Slack bot"));
    }
}

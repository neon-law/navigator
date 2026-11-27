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

use serde::{Deserialize, Serialize};

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

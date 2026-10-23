//! Worker-side [`Notifier`](workflows::Notifier) factory.
//!
//! The chat sibling of [`crate::email_config`]. Selection is driven by a single
//! optional env var, `SLACK_WEBHOOK_URL`:
//!
//! - set (non-empty) → [`workflows::SlackNotifier`] posting to that incoming
//!   webhook (the engineering channel in prod).
//! - unset / empty → [`workflows::CapturingNotifier`], so KIND and tests never
//!   post to a real channel.
//!
//! Unlike the email backend there is no hard failure mode: a missing webhook is
//! a valid (capturing) configuration, so the worker never crash-loops over it.

use std::sync::Arc;

use workflows::{CapturingSlackBot, Notifier, SlackBot, SlackBotClient};

/// Build the worker's notifier from the process environment.
#[must_use]
pub fn from_env() -> Arc<dyn Notifier> {
    from_lookup(|k| std::env::var(k).ok())
}

/// Testable seam: any `key -> Option<value>` lookup.
pub fn from_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Arc<dyn Notifier> {
    workflows::notify::notifier_from_lookup(get)
}

/// Whether the configured backend is the real Slack webhook (for boot logging).
#[must_use]
pub fn slack_enabled<F: Fn(&str) -> Option<String>>(get: F) -> bool {
    get("SLACK_WEBHOOK_URL").is_some_and(|s| !s.is_empty())
}

/// Build the target-aware Slack Web API client. Missing credentials select an
/// in-memory implementation so local KIND and tests never post externally.
pub fn bot_from_env() -> Result<Arc<dyn SlackBot>, String> {
    bot_from_lookup(|k| std::env::var(k).ok())
}

/// Testable seam for selecting the Slack bot backend.
///
/// `SLACK_API_BASE_URL` points the client at a local test double and is
/// honoured only under `NAVIGATOR_CI_HARNESS=1`; a hosted profile that sets it
/// is refused rather than silently ignored.
pub fn bot_from_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Result<Arc<dyn SlackBot>, String> {
    let base_url = workflows::harness_override(&get, "SLACK_API_BASE_URL")?;
    Ok(match get("SLACK_BOT_TOKEN").filter(|s| !s.is_empty()) {
        Some(token) => match base_url {
            Some(base_url) => Arc::new(SlackBotClient::with_base_url(token, base_url)),
            None => Arc::new(SlackBotClient::new(token)),
        },
        None => Arc::new(CapturingSlackBot::new()),
    })
}

/// Whether the target-aware Slack Web API backend is configured.
#[must_use]
pub fn bot_enabled<F: Fn(&str) -> Option<String>>(get: F) -> bool {
    get("SLACK_BOT_TOKEN").is_some_and(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{bot_enabled, bot_from_lookup, from_lookup, slack_enabled};
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[tokio::test]
    async fn capturing_when_webhook_unset() {
        assert!(!slack_enabled(|_| None));
        // The capturing backend always accepts a message.
        from_lookup(|_| None)
            .notify("dev".into())
            .await
            .expect("capturing backend always succeeds");
    }

    #[test]
    fn capturing_when_webhook_empty() {
        assert!(!slack_enabled(lookup(&[("SLACK_WEBHOOK_URL", "")])));
    }

    #[test]
    fn slack_when_webhook_set() {
        assert!(slack_enabled(lookup(&[(
            "SLACK_WEBHOOK_URL",
            "https://hooks.slack.com/services/T/B/x"
        )])));
    }

    #[test]
    fn bot_is_disabled_when_token_is_unset_or_empty() {
        assert!(!bot_enabled(|_| None));
        assert!(!bot_enabled(lookup(&[("SLACK_BOT_TOKEN", "")])));
    }

    #[tokio::test]
    async fn the_slack_api_override_requires_the_ci_harness() {
        let hosted = lookup(&[
            ("SLACK_BOT_TOKEN", "xoxb-test"),
            ("SLACK_API_BASE_URL", "http://fixture.invalid"),
        ]);
        assert!(bot_from_lookup(hosted).is_err());

        let harness = lookup(&[
            ("SLACK_BOT_TOKEN", "xoxb-test"),
            ("SLACK_API_BASE_URL", "http://fixture.invalid"),
            ("NAVIGATOR_CI_HARNESS", "1"),
        ]);
        assert!(bot_from_lookup(harness).is_ok());

        let no_override = lookup(&[("SLACK_BOT_TOKEN", "xoxb-test")]);
        assert!(bot_from_lookup(no_override).is_ok());
    }

    #[tokio::test]
    async fn bot_captures_when_token_is_unset() {
        assert!(!bot_enabled(|_| None));
        let bot = bot_from_lookup(|_| None).expect("capturing bot");
        let channel = bot
            .create_private_channel("sample-project")
            .await
            .expect("capturing Slack bot succeeds");
        bot.post_message(&channel.id, "test")
            .await
            .expect("capturing Slack bot succeeds");
    }
}

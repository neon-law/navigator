//! Internal Post notification identity and Slack destination selection.
//!
//! Callers resolve the Post's current Firm and Brand from the database before
//! preparing a delivery. Persist the prepared delivery before submitting its
//! stable key to Restate; a key alone does not prove provider acceptance.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::notify::{SlackBotError, SlackMessageReceipt};

/// An internal notification, distinct from a public social publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostNoticeEvent {
    ReviewRequested,
    PublicationResult,
}

/// Coordinates resolved from a Firm-authorized channel configuration.
pub struct PostNoticeChannels<'a> {
    pub firm_id: Uuid,
    pub production: Option<&'a str>,
    pub simulated: Option<&'a str>,
}

/// The safe destination for this Post's owning Firm and deployment.
#[must_use]
pub fn select_post_notice_channel<'a>(
    channels: &PostNoticeChannels<'a>,
    post_firm_id: Uuid,
    simulated: bool,
) -> Option<&'a str> {
    if channels.firm_id != post_firm_id {
        return None;
    }
    let selected = if simulated {
        channels.simulated
    } else {
        channels.production
    };
    selected.filter(|channel| {
        channel.starts_with('C')
            && channel.len() > 1
            && channel.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && (!simulated || Some(*channel) != channels.production)
    })
}

/// Identity of one notification of one saved Post revision to one destination.
/// Persist this identity before submitting its key to the durable worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostNoticeIdentity {
    pub firm_id: Uuid,
    pub post_id: Uuid,
    pub brand_id: Uuid,
    pub source_revision: String,
    pub event: PostNoticeEvent,
    pub channel_id: String,
    pub simulated: bool,
}

impl PostNoticeIdentity {
    /// Render only the event and internal Post coordinate, with no draft copy.
    /// The worker must resolve a configured origin rather than accepting a Post URL.
    #[must_use]
    pub fn render_notice(&self, configured_origin: &str) -> Option<String> {
        let mut url = reqwest::Url::parse(configured_origin).ok()?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        url.set_path("/app/admin/blog");
        let event = match self.event {
            PostNoticeEvent::ReviewRequested => "Post review requested",
            PostNoticeEvent::PublicationResult => "Post publication result recorded",
        };
        let label = if self.simulated { "[Simulated] " } else { "" };
        Some(format!(
            "{label}{event}. Post {}. <{url}|Open the Post dashboard>",
            self.post_id
        ))
    }

    /// A deterministic workflow key; it is not evidence that Slack accepted a message.
    #[must_use]
    pub fn delivery_key(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"post-slack-notice-v1");
        digest.update(self.firm_id.as_bytes());
        digest.update(self.post_id.as_bytes());
        digest.update(self.brand_id.as_bytes());
        for value in [&self.source_revision, &self.channel_id] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
        digest.update([
            match self.event {
                PostNoticeEvent::ReviewRequested => 0,
                PostNoticeEvent::PublicationResult => 1,
            },
            u8::from(self.simulated),
        ]);
        format!("post-slack-{}", URL_SAFE_NO_PAD.encode(digest.finalize()))
    }
}

/// What a durable caller may safely do after a provider attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostNoticeAttempt {
    Confirmed,
    RetryAfterRateLimit { seconds: Option<u64> },
    Rejected,
    ReconcileBeforeResend,
}

/// Lost replies can follow successful writes. Never blindly resend them.
#[must_use]
pub fn classify_post_notice_attempt(
    identity: &PostNoticeIdentity,
    result: &Result<SlackMessageReceipt, SlackBotError>,
) -> PostNoticeAttempt {
    match result {
        Ok(receipt)
            if receipt.channel_id == identity.channel_id && !receipt.timestamp.is_empty() =>
        {
            PostNoticeAttempt::Confirmed
        }
        Err(SlackBotError::HttpStatus(400 | 401 | 403 | 404)) => PostNoticeAttempt::Rejected,
        Ok(_)
        | Err(
            SlackBotError::Transport(_)
            | SlackBotError::IncompleteResponse
            | SlackBotError::RetryableHttpStatus(_)
            | SlackBotError::HttpStatus(_),
        ) => PostNoticeAttempt::ReconcileBeforeResend,
        Err(SlackBotError::RateLimited {
            retry_after_seconds,
        }) => PostNoticeAttempt::RetryAfterRateLimit {
            seconds: *retry_after_seconds,
        },
        Err(SlackBotError::Api(error))
            if matches!(
                error.as_str(),
                "request_timeout" | "internal_error" | "fatal_error"
            ) =>
        {
            PostNoticeAttempt::ReconcileBeforeResend
        }
        Err(SlackBotError::Api(_) | SlackBotError::MissingScope { .. }) => {
            PostNoticeAttempt::Rejected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> PostNoticeIdentity {
        PostNoticeIdentity {
            firm_id: Uuid::new_v4(),
            post_id: Uuid::new_v4(),
            brand_id: Uuid::new_v4(),
            source_revision: "digest-a".into(),
            event: PostNoticeEvent::ReviewRequested,
            channel_id: "CPRODUCTION".into(),
            simulated: false,
        }
    }

    #[test]
    fn notice_uses_only_internal_metadata_and_labels_simulation() {
        let mut notice = identity();
        notice.simulated = true;
        let text = notice
            .render_notice("https://navigator.example")
            .expect("valid origin");
        assert!(text.starts_with("[Simulated] Post review requested."));
        assert!(text.contains("https://navigator.example/app/admin/blog"));
        assert!(!text.contains(&notice.source_revision));
        for invalid in [
            "http://navigator.example",
            "https://user:secret@navigator.example",
            "https://navigator.example/path",
            "https://navigator.example?draft=secret",
        ] {
            assert_eq!(notice.render_notice(invalid), None);
        }
    }

    #[test]
    fn delivery_identity_changes_with_revision_event_destination_and_owner() {
        let original = identity();
        let key = original.delivery_key();
        assert_eq!(key, original.clone().delivery_key());
        let mut revisions = Vec::new();
        let mut changed = original.clone();
        changed.source_revision = "digest-b".into();
        revisions.push(changed);
        let mut changed = original.clone();
        changed.event = PostNoticeEvent::PublicationResult;
        revisions.push(changed);
        let mut changed = original.clone();
        changed.channel_id = "COTHER".into();
        revisions.push(changed);
        let mut changed = original.clone();
        changed.firm_id = Uuid::new_v4();
        revisions.push(changed);
        let mut changed = original.clone();
        changed.brand_id = Uuid::new_v4();
        revisions.push(changed);
        let mut changed = original.clone();
        changed.post_id = Uuid::new_v4();
        revisions.push(changed);
        let mut changed = original.clone();
        changed.simulated = true;
        revisions.push(changed);
        assert!(revisions
            .iter()
            .all(|changed| changed.delivery_key() != key));
    }

    #[test]
    fn simulated_destination_must_be_distinct_and_explicit() {
        let firm = Uuid::new_v4();
        let mut channels = PostNoticeChannels {
            firm_id: firm,
            production: Some("CPRODUCTION"),
            simulated: Some("CPRODUCTION"),
        };
        assert_eq!(select_post_notice_channel(&channels, firm, true), None);
        channels.simulated = Some("CTEST");
        assert_eq!(
            select_post_notice_channel(&channels, firm, true),
            Some("CTEST")
        );
        channels.simulated = Some("#messages");
        assert_eq!(select_post_notice_channel(&channels, firm, true), None);
    }

    #[test]
    fn lost_and_incomplete_responses_require_reconciliation() {
        let notice = identity();
        for error in [
            SlackBotError::Transport("lost response".into()),
            SlackBotError::IncompleteResponse,
            SlackBotError::HttpStatus(503),
            SlackBotError::HttpStatus(408),
            SlackBotError::Api("request_timeout".into()),
            SlackBotError::RetryableHttpStatus(502),
        ] {
            assert_eq!(
                classify_post_notice_attempt(&notice, &Err(error)),
                PostNoticeAttempt::ReconcileBeforeResend
            );
        }
        let receipt = SlackMessageReceipt {
            channel_id: notice.channel_id.clone(),
            timestamp: String::new(),
            correlation_id: None,
        };
        assert_eq!(
            classify_post_notice_attempt(&notice, &Ok(receipt)),
            PostNoticeAttempt::ReconcileBeforeResend
        );
    }

    #[test]
    fn receipt_requires_the_expected_destination_and_timestamp() {
        let notice = identity();
        let mut receipt = SlackMessageReceipt {
            channel_id: notice.channel_id.clone(),
            timestamp: "123.456".into(),
            correlation_id: None,
        };
        assert_eq!(
            classify_post_notice_attempt(&notice, &Ok(receipt.clone())),
            PostNoticeAttempt::Confirmed
        );
        receipt.channel_id = "COTHER".into();
        assert_eq!(
            classify_post_notice_attempt(&notice, &Ok(receipt)),
            PostNoticeAttempt::ReconcileBeforeResend
        );
        assert_eq!(
            classify_post_notice_attempt(
                &notice,
                &Err(SlackBotError::RateLimited {
                    retry_after_seconds: Some(30)
                })
            ),
            PostNoticeAttempt::RetryAfterRateLimit { seconds: Some(30) }
        );
        assert_eq!(
            classify_post_notice_attempt(
                &notice,
                &Err(SlackBotError::Api("channel_not_found".into()))
            ),
            PostNoticeAttempt::Rejected
        );
    }

    #[test]
    fn simulated_posts_never_fall_back_to_production() {
        let firm = Uuid::new_v4();
        let channels = PostNoticeChannels {
            firm_id: firm,
            production: Some("CPRODUCTION"),
            simulated: None,
        };
        assert_eq!(select_post_notice_channel(&channels, firm, true), None);
    }

    #[test]
    fn destination_is_scoped_to_the_posts_current_firm() {
        let channels = PostNoticeChannels {
            firm_id: Uuid::new_v4(),
            production: Some("CPRODUCTION"),
            simulated: Some("CTEST"),
        };
        assert_eq!(
            select_post_notice_channel(&channels, Uuid::new_v4(), false),
            None
        );
    }
}

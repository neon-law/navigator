//! Durable Slack notice for a newly persisted inbound Message.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessageAlertRequest {
    pub message_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessageAlertReport {
    pub message_id: Uuid,
    pub state: String,
}

/// Render only routing metadata. Message text, external names, and addresses
/// stay in the Firm inbox.
#[must_use]
pub fn render_inbound_message_alert(
    brand_key: &str,
    channel: &str,
    interaction_type: &str,
    inbox_url: &str,
) -> String {
    format!(
        "New inbound {interaction_type} on {channel} for Brand `{brand_key}`. <{inbox_url}|Open Inbox thread>."
    )
}

#[cfg(test)]
mod tests {
    use super::render_inbound_message_alert;

    #[test]
    fn alert_has_brand_channel_type_and_inbox_link_without_message_content() {
        let rendered = render_inbound_message_alert(
            "neon",
            "instagram",
            "comment",
            "https://navigator.example/app/inbox/synthetic",
        );
        assert!(rendered.contains("neon"));
        assert!(rendered.contains("instagram"));
        assert!(rendered.contains("comment"));
        assert!(rendered.contains("/app/inbox/synthetic"));
        assert!(!rendered.contains("sender@example.com"));
    }
}

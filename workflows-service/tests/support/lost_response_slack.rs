//! A Slack fake for the window between an accepted post and its checkpoint.

use std::sync::Mutex;

use workflows::{SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt};

/// A `SlackBot` whose first post reaches Slack and then never returns, so
/// the step's checkpoint never commits. History answers correlated lookups
/// from what was posted, as Slack's `client_msg_id` does.
#[derive(Default)]
pub struct LostResponseSlackBot {
    posted: Mutex<Vec<(String, Option<String>)>>,
    pub reached: tokio::sync::Notify,
}

impl LostResponseSlackBot {
    pub fn posted(&self) -> Vec<(String, Option<String>)> {
        self.posted.lock().expect("posted lock").clone()
    }

    async fn deliver(&self, channel: &str, correlation: Option<&str>) {
        let first = {
            let mut posted = self.posted.lock().expect("posted lock");
            posted.push((channel.to_owned(), correlation.map(str::to_owned)));
            posted.len() == 1
        };
        if first {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

#[async_trait::async_trait]
impl SlackBot for LostResponseSlackBot {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        Ok(SlackChannel {
            id: format!("C{}", name.to_uppercase().replace('-', "")),
            name: name.to_string(),
        })
    }

    async fn post_message(&self, channel: &str, _message: &str) -> Result<(), SlackBotError> {
        self.deliver(channel, None).await;
        Ok(())
    }

    async fn post_message_with_receipt(
        &self,
        channel: &str,
        _text: &str,
        correlation_id: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.deliver(channel, correlation_id).await;
        Ok(SlackMessageReceipt {
            channel_id: channel.to_owned(),
            timestamp: "1700000000.000100".to_owned(),
            correlation_id: correlation_id.map(str::to_owned),
        })
    }

    async fn message_receipt_by_correlation(
        &self,
        channel: &str,
        correlation_id: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        Ok(self
            .posted()
            .into_iter()
            .any(|(posted, key)| posted == channel && key.as_deref() == Some(correlation_id))
            .then(|| SlackMessageReceipt {
                channel_id: channel.to_owned(),
                timestamp: "1700000000.000100".to_owned(),
                correlation_id: Some(correlation_id.to_owned()),
            }))
    }

    async fn user_id_by_email(&self, _email: &str) -> Result<Option<String>, SlackBotError> {
        Ok(None)
    }

    async fn channel_member_ids(&self, _channel: &str) -> Result<Vec<String>, SlackBotError> {
        Ok(Vec::new())
    }

    async fn invite_to_channel(
        &self,
        _channel: &str,
        _user_ids: &[String],
    ) -> Result<(), SlackBotError> {
        Ok(())
    }
}

//! The Firm's own Slack bot for a Project's private channel (ENG-392).
//!
//! Slack is connected once per Firm: the Firm's Admin DRI stores a
//! `slack_bot_token` Firm credential, and every Project in that Firm reaches
//! its channel through that bot — the channel create-or-adopt, the main-push
//! notice, the Nightly Project Update's membership and canvas work. The worker
//! resolves the token from the Project's `firm_id` at the moment a lane needs
//! the bot and never falls back to another Firm's token. Work with no Project
//! in context posts with the deployment's Slack Firm through
//! [`workflows::firm_slack`]. The resolved value lives inside the bot client
//! and is never journaled, logged, or returned.

use std::sync::Arc;

use async_trait::async_trait;
use store::application_store::ApplicationStore;
use store::firm_secrets::{IntegrationProvider, IntegrationSecretKind, SecretStoreError};
use uuid::Uuid;
use workflows::{SlackBot, SlackBotClient};

use crate::project_slack::LaneError;

/// Which bot posts to a Project's private channel.
#[async_trait]
pub trait ProjectSlackBots: Send + Sync {
    /// The bot of the Project's owning Firm.
    ///
    /// # Errors
    /// [`LaneError::Refused`] when the Firm has not connected Slack or this
    /// deployment cannot decrypt Firm credentials — terminal, since a retry
    /// cannot store a token; [`LaneError::Store`] when the store or the KMS
    /// did not answer this attempt.
    async fn for_project(
        &self,
        application_store: &ApplicationStore,
        project_id: Uuid,
    ) -> Result<Arc<dyn SlackBot>, LaneError>;

    /// Native workers resolve credentials against their own explicit pool.
    /// Fakes may share the existing provider-only implementation.
    async fn for_project_on(
        &self,
        _pg: &store::postgres::PostgresDb,
        application_store: &ApplicationStore,
        project_id: Uuid,
    ) -> Result<Arc<dyn SlackBot>, LaneError> {
        self.for_project(application_store, project_id).await
    }
}

/// The production resolver: the Firm's `slack_bot_token`, unsealed with the
/// runtime KMS key, becomes a Web API client for one lane step.
pub struct FirmSlackBots {
    kms: Arc<dyn cloud::RuntimeKms>,
}

impl FirmSlackBots {
    #[must_use]
    pub fn new(kms: Arc<dyn cloud::RuntimeKms>) -> Self {
        Self { kms }
    }
}

#[async_trait]
impl ProjectSlackBots for FirmSlackBots {
    async fn for_project(
        &self,
        application_store: &ApplicationStore,
        project_id: Uuid,
    ) -> Result<Arc<dyn SlackBot>, LaneError> {
        let credential = store::firm_secrets::resolve_project_credential(
            application_store,
            project_id,
            IntegrationProvider::Slack,
            IntegrationSecretKind::SlackBotToken,
            self.kms.as_ref(),
        )
        .await
        .map_err(|error| credential_error(project_id, &error))?;
        Ok(Arc::new(SlackBotClient::new(
            credential.expose_for_provider_call(),
        )))
    }

    async fn for_project_on(
        &self,
        pg: &store::postgres::PostgresDb,
        _application_store: &ApplicationStore,
        project_id: Uuid,
    ) -> Result<Arc<dyn SlackBot>, LaneError> {
        let credential = store::firm_secrets::resolve_project_credential_on(
            pg,
            project_id,
            IntegrationProvider::Slack,
            IntegrationSecretKind::SlackBotToken,
            self.kms.as_ref(),
        )
        .await
        .map_err(|error| credential_error(project_id, &error))?;
        Ok(Arc::new(SlackBotClient::new(
            credential.expose_for_provider_call(),
        )))
    }
}

/// Classify a credential failure for the lane. Identifiers only: the message
/// names the Project and the provider, never a token or a Firm's data.
fn credential_error(project_id: Uuid, error: &SecretStoreError) -> LaneError {
    match error {
        SecretStoreError::Postgres => {
            LaneError::Store("Firm Slack credential lookup failed".to_owned())
        }
        SecretStoreError::Kms(
            cloud::KmsError::Unavailable | cloud::KmsError::Transport | cloud::KmsError::Http(_),
        ) => {
            LaneError::Store("Firm Slack credential could not be unsealed this attempt".to_string())
        }
        SecretStoreError::NoSuchProject(_) => {
            LaneError::ProjectGone("Project no longer exists for Slack notice".to_string())
        }
        other => {
            tracing::warn!(
                project_id = %project_id,
                reason = %other,
                "Slack is not connected for this Project's Firm; the notice was not posted"
            );
            LaneError::Refused(format!(
                "Slack is not connected for this Project's Firm: {other}"
            ))
        }
    }
}

/// Post `text` once per `correlation`: a step re-run after Slack accepted the
/// post but before its checkpoint finds the correlated receipt and does not
/// post again. The lookup reads recent history only, so a post that scrolled
/// out of it can repeat; delivery stays at-least-once, never lost.
///
/// # Errors
/// Slack refused the history read or the post.
pub(crate) async fn post_once(
    slack: &dyn SlackBot,
    channel: &str,
    correlation: &str,
    text: &str,
) -> Result<(), workflows::SlackBotError> {
    if slack
        .message_receipt_by_correlation(channel, correlation)
        .await?
        .is_none()
    {
        slack
            .post_message_with_receipt(channel, text, Some(correlation))
            .await?;
    }
    Ok(())
}

/// One bot for every Project. Tests and local fakes compose it around a
/// `CapturingSlackBot`; production never does, because a shared bot is the
/// deployment-wide fallback ENG-392 rules out.
pub struct SharedSlackBot(pub Arc<dyn SlackBot>);

#[async_trait]
impl ProjectSlackBots for SharedSlackBot {
    async fn for_project(
        &self,
        _application_store: &ApplicationStore,
        _project_id: Uuid,
    ) -> Result<Arc<dyn SlackBot>, LaneError> {
        Ok(Arc::clone(&self.0))
    }
}

#[cfg(test)]
mod tests {}

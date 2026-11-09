//! The Firm's own Slack bot for a Project's private channel (ENG-392).
//!
//! Slack is connected once per Firm: the Firm's Admin DRI stores a
//! `slack_bot_token` Firm credential, and every Project in that Firm reaches
//! its channel through that bot — the channel create-or-adopt, the main-push
//! notice, the Nightly Project Update's membership and canvas work. The worker
//! resolves the token from the Project's `firm_id` at the moment a lane needs
//! the bot and never falls back to another Firm's token or to a
//! deployment-wide `SLACK_BOT_TOKEN`; that token serves only the deployment's
//! own channels. The resolved value lives inside the bot client and is never
//! journaled, logged, or returned.

use std::sync::Arc;

use async_trait::async_trait;
use store::firm_secrets::{IntegrationProvider, IntegrationSecretKind, SecretStoreError};
use store::surreal::SurrealDb;
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
        surreal: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Arc<dyn SlackBot>, LaneError>;
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
        surreal: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Arc<dyn SlackBot>, LaneError> {
        let credential = store::firm_secrets::resolve_project_credential(
            surreal,
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
        SecretStoreError::Db(error) => {
            LaneError::Store(format!("Firm Slack credential lookup failed: {error}"))
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

/// One bot for every Project. Tests and local fakes compose it around a
/// `CapturingSlackBot`; production never does, because a shared bot is the
/// deployment-wide fallback ENG-392 rules out.
pub struct SharedSlackBot(pub Arc<dyn SlackBot>);

#[async_trait]
impl ProjectSlackBots for SharedSlackBot {
    async fn for_project(
        &self,
        _surreal: &SurrealDb,
        _project_id: Uuid,
    ) -> Result<Arc<dyn SlackBot>, LaneError> {
        Ok(Arc::clone(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::{FirmSlackBots, ProjectSlackBots, SharedSlackBot};
    use crate::project_slack::LaneError;
    use std::sync::Arc;
    use store::firm_secrets::{put, IntegrationProvider, IntegrationSecretKind, SecretPutRequest};
    use store::persons::Role;
    use store::projects::{create, NewProject};
    use store::test_support::{mem_surreal, seed_entity};
    use workflows::CapturingSlackBot;

    /// A Firm with its Admin DRI and one open Project: `(firm id, admin id,
    /// project)`.
    async fn firm_with_project(
        surreal: &store::surreal::SurrealDb,
        code: &str,
    ) -> (uuid::Uuid, uuid::Uuid, store::projects::Project) {
        let entity_id = seed_entity(surreal).await;
        let admin = store::persons::create(
            surreal,
            &store::persons::NewPerson {
                name: format!("Admin {code}"),
                email: format!("admin-{code}@example.com"),
                role: Role::Admin,
                ..Default::default()
            },
        )
        .await
        .expect("admin");
        let firm = store::firms::create(
            surreal,
            &store::firms::NewFirm {
                name: format!("Firm {code}"),
                status: "active".into(),
                entity_id,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .expect("firm");
        let project = create(
            surreal,
            &NewProject {
                code: code.to_string(),
                name: code.to_string(),
                status: "open".to_string(),
                entity_id,
                firm_id: Some(firm.id),
                ..Default::default()
            },
        )
        .await
        .expect("project");
        (firm.id, admin.id, project)
    }

    /// ENG-392's two-Firm proof: a Firm that connected Slack gets a bot for
    /// its Project; a Firm that did not is refused — never handed the other
    /// Firm's token, never a deployment-wide one.
    #[tokio::test]
    async fn each_project_resolves_only_its_own_firms_bot() {
        let surreal = mem_surreal().await;
        let kms = cloud::FakeKms::new(
            "projects/fixture/locations/global/keyRings/runtime/cryptoKeys/integration",
        );
        let (connected_firm, connected_admin, connected_project) =
            firm_with_project(&surreal, "connected").await;
        let (_, _, unconnected_project) = firm_with_project(&surreal, "unconnected").await;
        put(
            &surreal,
            SecretPutRequest {
                actor_role: Role::Admin,
                actor_person_id: Some(connected_admin),
                firm_id: connected_firm,
                provider: IntegrationProvider::Slack,
                kind: IntegrationSecretKind::SlackBotToken,
                value: "synthetic-slack-token-connected",
            },
            &kms,
        )
        .await
        .expect("store the connected Firm's token");

        let bots = FirmSlackBots::new(Arc::new(kms));
        bots.for_project(&surreal, connected_project.id)
            .await
            .expect("the connected Firm's Project gets a bot");
        let refused = bots
            .for_project(&surreal, unconnected_project.id)
            .await
            .err()
            .expect("a Firm without a token is refused");
        assert!(
            matches!(&refused, LaneError::Refused(message) if message.contains("not connected")),
            "{refused:?}"
        );
        assert!(refused.terminal(), "a missing token is not retried");
        assert!(
            !format!("{refused}").contains("synthetic-slack-token"),
            "no token value in the error"
        );
    }

    #[tokio::test]
    async fn a_deployment_without_a_runtime_kms_key_refuses_rather_than_falling_back() {
        let surreal = mem_surreal().await;
        let (_, _, project) = firm_with_project(&surreal, "sealed").await;
        let bots = FirmSlackBots::new(Arc::new(cloud::NoRuntimeKms));
        let refused = bots
            .for_project(&surreal, project.id)
            .await
            .err()
            .expect("no KMS, no bot");
        assert!(refused.terminal(), "{refused:?}");
    }

    #[tokio::test]
    async fn a_project_without_a_firm_or_row_is_refused() {
        let surreal = mem_surreal().await;
        let bots = FirmSlackBots::new(Arc::new(cloud::FakeKms::new("k")));
        let gone = bots
            .for_project(&surreal, uuid::Uuid::now_v7())
            .await
            .err()
            .expect("no row");
        assert!(matches!(gone, LaneError::ProjectGone(_)), "{gone:?}");
        let orphan = create(
            &surreal,
            &NewProject {
                code: "orphan".to_string(),
                name: "Orphan".to_string(),
                status: "open".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("project");
        let refused = bots
            .for_project(&surreal, orphan.id)
            .await
            .err()
            .expect("no Firm");
        assert!(refused.terminal(), "{refused:?}");
    }

    #[tokio::test]
    async fn the_shared_bot_serves_every_project() {
        let surreal = mem_surreal().await;
        let bot = Arc::new(CapturingSlackBot::new());
        let shared = SharedSlackBot(bot.clone());
        let resolved = shared
            .for_project(&surreal, uuid::Uuid::now_v7())
            .await
            .expect("shared");
        resolved.post_message("C1", "hi").await.expect("post");
        assert_eq!(bot.posted_messages().len(), 1);
    }
}

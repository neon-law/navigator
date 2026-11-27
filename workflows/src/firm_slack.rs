//! The one Slack seam.
//!
//! [`SlackBot`] is the port every Slack call goes through: digests, nags,
//! alerts, notices, channel provisioning, membership and canvases. Its only
//! live adapter is a Firm's own bot — the Firm's `slack_bot_token` Firm
//! Integration Secret, unsealed with the runtime KMS key when a call needs it.
//! No deployment-wide token or incoming webhook exists.
//!
//! Project lanes resolve the bot of the Project's owning Firm. Work with no
//! Firm in context (deployment digests, nags, ops notices) posts with the
//! deployment's Slack Firm: [`SLACK_FIRM_ENV`] when set, otherwise the Firm
//! wearing the `neon` Brand.

use std::sync::Arc;

use async_trait::async_trait;
use store::application_store::ApplicationStore;
use store::firm_secrets::{IntegrationProvider, IntegrationSecretKind, SecretStoreError};
use uuid::Uuid;

use crate::notify::{
    CapturingSlackBot, SlackBot, SlackBotClient, SlackBotError, SlackChannel, SlackMessageReceipt,
};

/// The Firm whose bot owns this deployment's own channels.
pub const SLACK_FIRM_ENV: &str = "SLACK_FIRM_ID";

/// The Brand whose Firm owns the deployment's channels when
/// [`SLACK_FIRM_ENV`] is unset.
const DEFAULT_SLACK_BRAND: &str = "neon";

/// The Slack error code a Firm without a usable bot reports.
pub const FIRM_SLACK_NOT_CONNECTED: &str = "firm_slack_not_connected";

/// Build a Web API client from a Firm's own bot token.
///
/// # Errors
/// The Firm has no active `slack_bot_token`, the runtime KMS cannot unseal
/// it, or the store did not answer.
pub async fn firm_bot(
    application_store: &ApplicationStore,
    firm_id: Uuid,
    kms: &dyn cloud::RuntimeKms,
    base_url: Option<&str>,
) -> Result<SlackBotClient, SecretStoreError> {
    let credential = store::firm_secrets::resolve_firm_credential(
        application_store,
        firm_id,
        IntegrationProvider::Slack,
        IntegrationSecretKind::SlackBotToken,
        kms,
    )
    .await?;
    let token = credential.expose_for_provider_call();
    Ok(match base_url {
        Some(base_url) => SlackBotClient::with_base_url(token, base_url),
        None => SlackBotClient::new(token),
    })
}

/// Classify a credential failure as a Slack error. Store and KMS outages are
/// transient; a Firm that never connected Slack is a refusal. The message
/// names no token.
#[must_use]
pub fn credential_error(error: &SecretStoreError) -> SlackBotError {
    match error {
        SecretStoreError::Postgres
        | SecretStoreError::Kms(
            cloud::KmsError::Unavailable | cloud::KmsError::Transport | cloud::KmsError::Http(_),
        ) => SlackBotError::Transport("Firm Slack credential is unavailable".to_owned()),
        _ => SlackBotError::Api(FIRM_SLACK_NOT_CONNECTED.to_owned()),
    }
}

/// The deployment's Slack Firm, read from [`SLACK_FIRM_ENV`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentSlackFirm {
    /// The configured Firm.
    Configured(Uuid),
    /// The Firm wearing the default Brand.
    DefaultBrand,
}

impl DeploymentSlackFirm {
    /// # Errors
    /// [`SLACK_FIRM_ENV`] is set to something other than a Firm id.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        match lookup(SLACK_FIRM_ENV)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            Some(value) => Uuid::parse_str(&value)
                .map(Self::Configured)
                .map_err(|_| format!("{SLACK_FIRM_ENV} is not a Firm id")),
            None => Ok(Self::DefaultBrand),
        }
    }

    /// The Firm id, or `None` when no Firm wears the default Brand.
    ///
    /// # Errors
    /// The store did not answer.
    pub async fn resolve(
        self,
        application_store: &ApplicationStore,
    ) -> Result<Option<Uuid>, SlackBotError> {
        match self {
            Self::Configured(firm_id) => Ok(Some(firm_id)),
            Self::DefaultBrand => {
                store::firms::firm_id_for_brand_key(application_store, DEFAULT_SLACK_BRAND)
                    .await
                    .map_err(|_| SlackBotError::Transport("Firm lookup failed".to_owned()))
            }
        }
    }
}

/// The deployment's own bot: the Slack Firm's bot, resolved per call.
pub struct FirmSlackBot {
    application_store: ApplicationStore,
    kms: Arc<dyn cloud::RuntimeKms>,
    firm: DeploymentSlackFirm,
    base_url: Option<String>,
}

impl FirmSlackBot {
    #[must_use]
    pub fn new(
        application_store: ApplicationStore,
        kms: Arc<dyn cloud::RuntimeKms>,
        firm: DeploymentSlackFirm,
        base_url: Option<String>,
    ) -> Self {
        Self {
            application_store,
            kms,
            firm,
            base_url,
        }
    }

    async fn bot(&self) -> Result<SlackBotClient, SlackBotError> {
        let firm_id = self
            .firm
            .resolve(&self.application_store)
            .await?
            .ok_or_else(|| SlackBotError::Api(FIRM_SLACK_NOT_CONNECTED.to_owned()))?;
        firm_bot(
            &self.application_store,
            firm_id,
            self.kms.as_ref(),
            self.base_url.as_deref(),
        )
        .await
        .map_err(|error| credential_error(&error))
    }
}

/// The deployment's bot for a host, or `None` when nothing may post. A CI
/// harness that points `SLACK_API_BASE_URL` at its fake posts there with a
/// synthetic token; a host without a runtime KMS key (local, KIND) has no
/// bot; every other host posts with its Slack Firm's bot.
///
/// # Errors
/// An invalid [`SLACK_FIRM_ENV`], or `SLACK_API_BASE_URL` outside the harness.
pub fn deployment_bot(
    application_store: ApplicationStore,
    kms: Arc<dyn cloud::RuntimeKms>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<Arc<dyn SlackBot>>, String> {
    let firm = DeploymentSlackFirm::from_lookup(&lookup)?;
    if let Some(base_url) = crate::harness_override(&lookup, "SLACK_API_BASE_URL")? {
        return Ok(Some(Arc::new(SlackBotClient::with_base_url(
            "ci-harness",
            base_url,
        ))));
    }
    let sealed = lookup(cloud::RUNTIME_KMS_KEY_ENV).is_some_and(|key| !key.trim().is_empty());
    Ok(sealed.then(|| {
        Arc::new(FirmSlackBot::new(application_store, kms, firm, None)) as Arc<dyn SlackBot>
    }))
}

/// [`deployment_bot`], capturing when nothing may post, and whether it posts.
///
/// # Errors
/// As [`deployment_bot`].
pub fn deployment_bot_or_capture(
    application_store: ApplicationStore,
    kms: Arc<dyn cloud::RuntimeKms>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<(Arc<dyn SlackBot>, bool), String> {
    Ok(match deployment_bot(application_store, kms, lookup)? {
        Some(bot) => (bot, true),
        None => (Arc::new(CapturingSlackBot::new()), false),
    })
}

#[async_trait]
impl SlackBot for FirmSlackBot {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.bot().await?.create_private_channel(name).await
    }

    async fn find_private_channel_by_name(
        &self,
        name: &str,
    ) -> Result<Option<SlackChannel>, SlackBotError> {
        self.bot().await?.find_private_channel_by_name(name).await
    }

    async fn post_message(&self, channel_id: &str, text: &str) -> Result<(), SlackBotError> {
        self.bot().await?.post_message(channel_id, text).await
    }

    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
        self.bot().await?.user_id_by_email(email).await
    }

    async fn channel_member_ids(&self, channel_id: &str) -> Result<Vec<String>, SlackBotError> {
        self.bot().await?.channel_member_ids(channel_id).await
    }

    async fn invite_to_channel(
        &self,
        channel_id: &str,
        user_ids: &[String],
    ) -> Result<(), SlackBotError> {
        self.bot()
            .await?
            .invite_to_channel(channel_id, user_ids)
            .await
    }

    async fn remove_from_channel(
        &self,
        channel_id: &str,
        user_id: &str,
    ) -> Result<bool, SlackBotError> {
        self.bot()
            .await?
            .remove_from_channel(channel_id, user_id)
            .await
    }

    async fn post_message_with_receipt(
        &self,
        channel_id: &str,
        text: &str,
        correlation_id: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.bot()
            .await?
            .post_message_with_receipt(channel_id, text, correlation_id)
            .await
    }

    async fn post_blocks(
        &self,
        channel_id: &str,
        blocks: &[serde_json::Value],
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        self.bot().await?.post_blocks(channel_id, blocks).await
    }

    async fn message_receipt_by_correlation(
        &self,
        channel_id: &str,
        correlation_id: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        self.bot()
            .await?
            .message_receipt_by_correlation(channel_id, correlation_id)
            .await
    }

    async fn message_receipt_by_correlation_since(
        &self,
        channel_id: &str,
        correlation_id: &str,
        oldest: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        self.bot()
            .await?
            .message_receipt_by_correlation_since(channel_id, correlation_id, oldest)
            .await
    }

    async fn post_thread_reply(
        &self,
        channel_id: &str,
        thread_ts: &str,
        text: &str,
    ) -> Result<(), SlackBotError> {
        self.bot()
            .await?
            .post_thread_reply(channel_id, thread_ts, text)
            .await
    }

    async fn channel_canvas_id(&self, channel_id: &str) -> Result<Option<String>, SlackBotError> {
        self.bot().await?.channel_canvas_id(channel_id).await
    }

    async fn create_channel_canvas(
        &self,
        channel_id: &str,
        markdown: &str,
    ) -> Result<String, SlackBotError> {
        self.bot()
            .await?
            .create_channel_canvas(channel_id, markdown)
            .await
    }

    async fn replace_canvas(&self, canvas_id: &str, markdown: &str) -> Result<(), SlackBotError> {
        self.bot().await?.replace_canvas(canvas_id, markdown).await
    }

    async fn archive_channel(&self, channel_id: &str) -> Result<bool, SlackBotError> {
        self.bot().await?.archive_channel(channel_id).await
    }

    async fn unarchive_channel(&self, channel_id: &str) -> Result<bool, SlackBotError> {
        self.bot().await?.unarchive_channel(channel_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        deployment_bot, deployment_bot_or_capture, DeploymentSlackFirm, FirmSlackBot,
        FIRM_SLACK_NOT_CONNECTED,
    };
    use crate::notify::{SlackBot, SlackBotError};
    use std::sync::Arc;
    use store::application_store::ApplicationStore;
    use store::firm_secrets::{put, IntegrationProvider, IntegrationSecretKind, SecretPutRequest};
    use store::persons::Role;
    use uuid::Uuid;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "projects/fixture/locations/global/keyRings/runtime/cryptoKeys/integration";

    /// A Firm wearing `brand`, holding `token` as its bot when given.
    async fn firm(
        application_store: &ApplicationStore,
        kms: &cloud::FakeKms,
        brand: &str,
        token: Option<&str>,
    ) -> Uuid {
        let (firm, admin) =
            store::test_support::seed_firm_with_new_brand(application_store, brand).await;
        if let Some(value) = token {
            put(
                application_store,
                SecretPutRequest {
                    actor_role: Role::Admin,
                    actor_person_id: Some(admin),
                    firm_id: firm,
                    provider: IntegrationProvider::Slack,
                    kind: IntegrationSecretKind::SlackBotToken,
                    value,
                },
                kms,
            )
            .await
            .unwrap();
        }
        firm
    }

    async fn slack_expecting(token: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"ok": true, "channel": "C0OPS", "ts": "1.2"}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn the_deployment_bot_posts_with_its_slack_firms_own_token() {
        store::test_support::with_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let kms = cloud::FakeKms::new(KEY);
            firm(&application_store, &kms, "neon", Some("xoxb-neon")).await;
            let other = firm(&application_store, &kms, "other", Some("xoxb-other")).await;

            let server = slack_expecting("xoxb-neon").await;
            let default = FirmSlackBot::new(
                application_store.clone(),
                Arc::new(kms.clone()),
                DeploymentSlackFirm::DefaultBrand,
                Some(server.uri()),
            );
            default.post_message("C0OPS", "digest").await.unwrap();

            let server = slack_expecting("xoxb-other").await;
            let configured = FirmSlackBot::new(
                application_store,
                Arc::new(kms),
                DeploymentSlackFirm::Configured(other),
                Some(server.uri()),
            );
            configured.post_message("C0OPS", "digest").await.unwrap();
        })
        .await;
    }

    #[tokio::test]
    async fn a_firm_without_a_bot_is_refused_without_calling_slack() {
        store::test_support::with_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let kms = cloud::FakeKms::new(KEY);
            let unconnected = firm(&application_store, &kms, "neon", None).await;
            for firm in [
                DeploymentSlackFirm::Configured(unconnected),
                DeploymentSlackFirm::Configured(Uuid::now_v7()),
            ] {
                let bot = FirmSlackBot::new(
                    application_store.clone(),
                    Arc::new(kms.clone()),
                    firm,
                    Some("http://127.0.0.1:9".into()),
                );
                let error = bot.post_message("C0OPS", "digest").await.unwrap_err();
                assert!(
                    matches!(&error, SlackBotError::Api(code) if code == FIRM_SLACK_NOT_CONNECTED),
                    "{error:?}"
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn hosts_select_firm_capture_or_harness_and_reject_bad_configuration() {
        let application_store = ApplicationStore::uninitialized;
        let kms = || Arc::new(cloud::NoRuntimeKms) as Arc<dyn cloud::RuntimeKms>;
        let get = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        let (captured, live) =
            deployment_bot_or_capture(application_store(), kms(), get(&[])).unwrap();
        assert!(!live);
        assert!(captured.post_message("C0OPS", "captured").await.is_ok());
        let sealed = &[(
            cloud::RUNTIME_KMS_KEY_ENV,
            "projects/p/locations/l/keyRings/r/cryptoKeys/k",
        )];
        assert!(deployment_bot(application_store(), kms(), get(sealed))
            .unwrap()
            .is_some());
        assert!(deployment_bot(
            application_store(),
            kms(),
            get(&[("SLACK_FIRM_ID", "neon")])
        )
        .is_err());
        assert!(deployment_bot(
            application_store(),
            kms(),
            get(&[("SLACK_API_BASE_URL", "http://127.0.0.1:9")])
        )
        .is_err());
        assert!(deployment_bot(
            application_store(),
            kms(),
            get(&[
                ("NAVIGATOR_CI_HARNESS", "1"),
                ("SLACK_API_BASE_URL", "http://127.0.0.1:9")
            ])
        )
        .unwrap()
        .is_some());
        assert_eq!(
            DeploymentSlackFirm::from_lookup(get(&[(
                "SLACK_FIRM_ID",
                "00000000-0000-7000-8000-000000000001"
            )])),
            Ok(DeploymentSlackFirm::Configured(
                Uuid::parse_str("00000000-0000-7000-8000-000000000001").unwrap()
            ))
        );
    }
}

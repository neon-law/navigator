//! Resolving a Firm's provider client for one Project.
//!
//! The provider adapters in `cloud` take a token; the credential lives as an
//! encrypted row keyed by Firm. This module is the seam between them, and it
//! is a factory rather than a single injected client for one reason: the
//! token differs per Project, because it is resolved from that Project's
//! owning Firm. A process-wide `Arc<dyn NotionService>` would have to hold
//! one deployment-wide token, which is the arrangement the Firm-secret
//! boundary exists to replace.
//!
//! Errors here are mechanism-only. A caller learns that a credential is not
//! configured, that the Project has no owning Firm, or that the provider
//! refused — never the token, the ciphertext, or the provider's own body. An
//! unconfigured runtime names the deployment variable it lacks and the
//! operator step that supplies it: those are deployment coordinates, not
//! credentials, and naming them is what lets an operator act on the outcome.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use uuid::Uuid;

use store::firm_secrets::{IntegrationProvider, IntegrationSecretKind};
use store::surreal::SurrealDb;

/// Why a Project's provider client could not be built or used.
#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    #[error("this deployment's integration runtime is not configured: {0}")]
    NotConfigured(String),
    #[error("the Firm has not configured this provider credential")]
    CredentialMissing,
    #[error("the project has no owning Firm")]
    NoFirm,
    #[error("no such project")]
    NoSuchProject,
    #[error("the stored credential could not be produced")]
    CredentialUnusable,
    #[error("the provider refused or was unreachable")]
    Provider,
    #[error("database: {0}")]
    Db(String),
}

impl IntegrationError {
    /// The stable slug a door reports. Distinct from the message so a caller
    /// can branch on the outcome without matching prose.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Self::NotConfigured(_) => "runtime_not_configured",
            Self::CredentialMissing => "credential_missing",
            Self::CredentialUnusable => "credential_unusable",
            Self::NoFirm => "no_owning_firm",
            Self::NoSuchProject => "no_such_project",
            Self::Provider => "provider_unavailable",
            Self::Db(_) => "database_error",
        }
    }

    /// The operator-facing detail a door reports beside the slug: the missing
    /// deployment configuration and the step that supplies it. Only the
    /// unconfigured runtime carries one — every other outcome either names
    /// its own remedy (`credential_missing`) or would have to describe a
    /// credential to say more.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::NotConfigured(detail) => Some(detail),
            _ => None,
        }
    }
}

/// Where the operator steps for every runtime gap below are written down.
const RUNTIME_DOC: &str = "docs/third-party-integrations.md#provisioning-the-integration-runtime";

/// Which piece of the Firm integration runtime (Notion and Slack) a
/// deployment lacks. [`from_env`] reads them in this order, so the first gap
/// is the one reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FirmRuntimeGap {
    /// `NAVIGATOR_RUNTIME_KMS_KEY` is unset — every local checkout and KIND
    /// cluster.
    #[default]
    RuntimeKmsKey,
    /// The runtime key is set but `NAVIGATOR_NOTION_DATABASE_ID` is not.
    NotionDatabase,
    /// Both coordinates are set but no application-default credential
    /// resolved for the workload.
    WorkloadCredential,
}

impl FirmRuntimeGap {
    /// The missing configuration and the operator step that supplies it.
    #[must_use]
    pub fn detail(self) -> String {
        match self {
            Self::RuntimeKmsKey => format!(
                "NAVIGATOR_RUNTIME_KMS_KEY is unset; Notion and Slack need it and \
                 NAVIGATOR_NOTION_DATABASE_ID in the deployment's secrets.enc.yaml, then \
                 `navigator ops secrets apply --deployment <row>` and \
                 `navigator ops ship --deployment <row> --restart-only` (see {RUNTIME_DOC})"
            ),
            Self::NotionDatabase => format!(
                "NAVIGATOR_NOTION_DATABASE_ID is unset; add it to the deployment's \
                 secrets.enc.yaml beside NAVIGATOR_RUNTIME_KMS_KEY, then \
                 `navigator ops secrets apply --deployment <row>` and \
                 `navigator ops ship --deployment <row> --restart-only` (see {RUNTIME_DOC})"
            ),
            Self::WorkloadCredential => format!(
                "no application-default credential resolved for NAVIGATOR_RUNTIME_KMS_KEY; \
                 bind the web pod's Kubernetes service account to the runtime GCP service \
                 account through Workload Identity and grant it \
                 roles/cloudkms.cryptoKeyEncrypterDecrypter on that key (see {RUNTIME_DOC})"
            ),
        }
    }
}

/// The detail for a calendar runtime missing `missing` of
/// [`cloud::calendar::CALENDAR_CONFIGURATION_KEYS`].
fn calendar_gap(missing: &str) -> String {
    format!(
        "{missing} unset; the Project calendar uses the Workspace service account Drive uses: \
         set NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER in the deployment's config.toml and \
         NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON in its secrets.enc.yaml, authorize \
         the account's client id for https://www.googleapis.com/auth/calendar under \
         domain-wide delegation, then `navigator ops secrets apply --deployment <row>` \
         (see {RUNTIME_DOC})"
    )
}

/// Map the deployment's calendar configuration to a provider or the outcome
/// that names what is missing. A half configuration is unconfigured, not an
/// unusable credential: the operator's remedy is the missing key.
fn calendar_from_resolution(
    resolution: Result<Option<cloud::GoogleCalendar>, cloud::CalendarError>,
) -> Result<Arc<dyn cloud::CalendarService>, IntegrationError> {
    match resolution {
        Ok(Some(calendar)) => Ok(Arc::new(calendar)),
        Ok(None) => Err(IntegrationError::NotConfigured(calendar_gap(
            &cloud::calendar::CALENDAR_CONFIGURATION_KEYS.join(" and "),
        ))),
        Err(cloud::CalendarError::MissingConfig(missing)) => {
            Err(IntegrationError::NotConfigured(calendar_gap(&missing)))
        }
        Err(_) => Err(IntegrationError::CredentialUnusable),
    }
}

fn secret_error(error: store::firm_secrets::SecretStoreError) -> IntegrationError {
    use store::firm_secrets::SecretStoreError as Error;
    match error {
        Error::NotConfigured { .. } => IntegrationError::CredentialMissing,
        Error::ProjectHasNoFirm => IntegrationError::NoFirm,
        Error::NoSuchProject(_) => IntegrationError::NoSuchProject,
        Error::Db(error) => IntegrationError::Db(error.to_string()),
        // A KMS fault, a context mismatch, or a payload that will not decode
        // are a credential that exists and cannot be produced — a different
        // operator action from one that was never written, so a different
        // outcome. None of them may carry their detail outward: the detail is
        // about ciphertext.
        _ => IntegrationError::CredentialUnusable,
    }
}

/// Build one Project's provider client from its Firm's stored credential.
#[async_trait]
pub trait IntegrationProviders: Send + Sync {
    async fn notion(
        &self,
        surreal: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Arc<dyn cloud::NotionService>, IntegrationError>;

    async fn slack(
        &self,
        surreal: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Arc<dyn cloud::SlackService>, IntegrationError>;

    /// The Workspace calendar a Project's shared calendar lives on.
    ///
    /// Unlike Notion and Slack this is not a per-Firm credential: the calendar
    /// is owned by the deployment's Workspace service account, the one Drive
    /// already uses, so every resolver reads the same deployment
    /// configuration. A deployment with none of it set is `NotConfigured`.
    async fn calendar(
        &self,
        _surreal: &SurrealDb,
        _project_id: Uuid,
    ) -> Result<Arc<dyn cloud::CalendarService>, IntegrationError> {
        calendar_from_resolution(store::project_calendar::calendar_from_env().await)
    }
}

/// The resolver for a deployment missing part of the Firm integration
/// runtime. The default is the missing runtime KMS key — every local
/// checkout and every KIND cluster.
///
/// It refuses rather than falling back to an environment token. A fallback is
/// exactly the shape the Firm-secret boundary forbids: it would let a Project
/// reach a credential its Firm never wrote.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredIntegrations {
    gap: FirmRuntimeGap,
}

impl UnconfiguredIntegrations {
    #[must_use]
    pub const fn missing(gap: FirmRuntimeGap) -> Self {
        Self { gap }
    }

    fn refusal(self) -> IntegrationError {
        IntegrationError::NotConfigured(self.gap.detail())
    }
}

#[async_trait]
impl IntegrationProviders for UnconfiguredIntegrations {
    async fn notion(
        &self,
        _surreal: &SurrealDb,
        _project_id: Uuid,
    ) -> Result<Arc<dyn cloud::NotionService>, IntegrationError> {
        Err(self.refusal())
    }

    async fn slack(
        &self,
        _surreal: &SurrealDb,
        _project_id: Uuid,
    ) -> Result<Arc<dyn cloud::SlackService>, IntegrationError> {
        Err(self.refusal())
    }
}

/// The production resolver: one KMS handle and one parent Notion database,
/// with the per-Firm token read fresh for each call.
///
/// The credential is deliberately not cached. A revoked credential has to
/// stop working on the next call, and `firm_secrets::revoke` writes a row —
/// it does not call the provider — so a cache would keep a revoked token
/// live for as long as it survived.
pub struct FirmIntegrations {
    kms: Arc<dyn cloud::RuntimeKms>,
    notion_database_id: String,
}

impl std::fmt::Debug for FirmIntegrations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FirmIntegrations")
            .field("notion_database_id", &self.notion_database_id)
            .finish_non_exhaustive()
    }
}

impl FirmIntegrations {
    #[must_use]
    pub fn new(kms: Arc<dyn cloud::RuntimeKms>, notion_database_id: impl Into<String>) -> Self {
        Self {
            kms,
            notion_database_id: notion_database_id.into(),
        }
    }

    async fn credential(
        &self,
        surreal: &SurrealDb,
        project_id: Uuid,
        provider: IntegrationProvider,
        kind: IntegrationSecretKind,
    ) -> Result<String, IntegrationError> {
        let credential = store::firm_secrets::resolve_project_credential(
            surreal,
            project_id,
            provider,
            kind,
            self.kms.as_ref(),
        )
        .await
        .map_err(secret_error)?;
        Ok(credential.expose_for_provider_call().to_string())
    }
}

#[async_trait]
impl IntegrationProviders for FirmIntegrations {
    async fn notion(
        &self,
        surreal: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Arc<dyn cloud::NotionService>, IntegrationError> {
        let token = self
            .credential(
                surreal,
                project_id,
                IntegrationProvider::Notion,
                IntegrationSecretKind::NotionToken,
            )
            .await?;
        Ok(Arc::new(cloud::NotionClient::new(
            token,
            self.notion_database_id.clone(),
        )))
    }

    async fn slack(
        &self,
        surreal: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Arc<dyn cloud::SlackService>, IntegrationError> {
        let token = self
            .credential(
                surreal,
                project_id,
                IntegrationProvider::Slack,
                IntegrationSecretKind::SlackBotToken,
            )
            .await?;
        Ok(Arc::new(cloud::SlackClient::new(token)))
    }
}

/// Build the runtime KMS client from the environment, or the fail-closed
/// [`cloud::NoRuntimeKms`] arm when no dedicated key is configured or no
/// workload-identity credential resolves. Shared by [`from_env`] (the
/// resolver) and the Firm settings door (`portal::admin`'s
/// create/replace/revoke handlers), which needs the same KMS handle without
/// also requiring a Notion database id.
pub async fn runtime_kms_from_env() -> Arc<dyn cloud::RuntimeKms> {
    cloud::runtime_kms_from_env().await
}

/// Select the resolver from the environment.
///
/// Both coordinates are required together. A runtime KMS key with no parent
/// Notion database would build a resolver whose Notion arm cannot create a
/// page, and a database id with no KMS key cannot decrypt a token, so a half
/// configuration is refused as unconfigured rather than half-working.
pub async fn from_env() -> Arc<dyn IntegrationProviders> {
    let Ok(config) = cloud::GoogleKmsConfig::from_env() else {
        tracing::info!("firm integrations: unconfigured (NAVIGATOR_RUNTIME_KMS_KEY unset)");
        return Arc::new(UnconfiguredIntegrations::missing(
            FirmRuntimeGap::RuntimeKmsKey,
        ));
    };
    let Ok(database) = cloud::NotionDatabaseConfig::from_lookup(|key| std::env::var(key).ok())
    else {
        tracing::info!("firm integrations: unconfigured (NAVIGATOR_NOTION_DATABASE_ID unset)");
        return Arc::new(UnconfiguredIntegrations::missing(
            FirmRuntimeGap::NotionDatabase,
        ));
    };
    let Ok(tokens) = cloud::AdcTokenSource::new().await else {
        tracing::warn!(
            "firm integrations: unconfigured (a runtime KMS key is set but no \
             application-default credential resolved)"
        );
        return Arc::new(UnconfiguredIntegrations::missing(
            FirmRuntimeGap::WorkloadCredential,
        ));
    };
    tracing::info!("firm integrations: FirmIntegrations (runtime KMS + Notion database)");
    Arc::new(FirmIntegrations::new(
        Arc::new(cloud::GoogleKms::new(config, Arc::new(tokens))),
        database.database_id,
    ))
}

/// In-process providers for dev and tests. One shared fake per provider, so a
/// test drives the door and then reads what the provider saw.
#[derive(Debug, Default, Clone)]
pub struct FakeIntegrations {
    pub notion: cloud::FakeNotion,
    pub slack: cloud::FakeSlack,
    pub calendar: cloud::FakeCalendar,
    notion_calls: Arc<AtomicUsize>,
    slack_calls: Arc<AtomicUsize>,
    calendar_calls: Arc<AtomicUsize>,
}

impl FakeIntegrations {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn notion_calls(&self) -> usize {
        self.notion_calls.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn slack_calls(&self) -> usize {
        self.slack_calls.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn calendar_calls(&self) -> usize {
        self.calendar_calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl IntegrationProviders for FakeIntegrations {
    async fn notion(
        &self,
        _surreal: &SurrealDb,
        _project_id: Uuid,
    ) -> Result<Arc<dyn cloud::NotionService>, IntegrationError> {
        self.notion_calls.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(self.notion.clone()))
    }

    async fn slack(
        &self,
        _surreal: &SurrealDb,
        _project_id: Uuid,
    ) -> Result<Arc<dyn cloud::SlackService>, IntegrationError> {
        self.slack_calls.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(self.slack.clone()))
    }

    async fn calendar(
        &self,
        _surreal: &SurrealDb,
        _project_id: Uuid,
    ) -> Result<Arc<dyn cloud::CalendarService>, IntegrationError> {
        self.calendar_calls.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(self.calendar.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        calendar_from_resolution, FirmRuntimeGap, IntegrationError, IntegrationProviders,
        UnconfiguredIntegrations,
    };
    use uuid::Uuid;

    /// A deployment with no runtime key refuses rather than reaching for a
    /// deployment-wide provider token, and the refusal names the variable the
    /// operator has to set. The refusal is the contract: a Project must never
    /// reach a credential its Firm did not write.
    #[tokio::test]
    async fn an_unconfigured_deployment_refuses_both_providers_naming_the_missing_key() {
        let surreal = store::test_support::mem_surreal().await;
        let providers = UnconfiguredIntegrations::default();
        for error in [
            providers.notion(&surreal, Uuid::now_v7()).await.err(),
            providers.slack(&surreal, Uuid::now_v7()).await.err(),
        ] {
            let error = error.expect("an unconfigured runtime refuses");
            assert_eq!(error.slug(), "runtime_not_configured");
            let detail = error.detail().expect("the refusal names its gap");
            assert!(detail.contains("NAVIGATOR_RUNTIME_KMS_KEY"), "{detail}");
            assert!(detail.contains("navigator ops secrets apply"), "{detail}");
        }
    }

    #[test]
    fn each_firm_runtime_gap_names_its_own_configuration() {
        assert!(FirmRuntimeGap::NotionDatabase
            .detail()
            .starts_with("NAVIGATOR_NOTION_DATABASE_ID is unset"));
        assert!(FirmRuntimeGap::WorkloadCredential
            .detail()
            .contains("Workload Identity"));
    }

    #[test]
    fn an_unconfigured_calendar_names_the_missing_workspace_keys() {
        let error = calendar_from_resolution(Ok(None)).err().unwrap();
        assert_eq!(error.slug(), "runtime_not_configured");
        let detail = error.detail().unwrap();
        assert!(detail.starts_with(
            "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER and \
             NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON unset"
        ));

        let half = calendar_from_resolution(Err(cloud::CalendarError::MissingConfig(
            "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON".into(),
        )))
        .err()
        .unwrap();
        assert_eq!(half.slug(), "runtime_not_configured");
        assert!(half
            .detail()
            .unwrap()
            .starts_with("NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON unset"));
    }

    #[test]
    fn every_error_carries_a_slug_and_no_credential_detail() {
        for error in [
            IntegrationError::NotConfigured(FirmRuntimeGap::RuntimeKmsKey.detail()),
            IntegrationError::NotConfigured(FirmRuntimeGap::NotionDatabase.detail()),
            IntegrationError::NotConfigured(FirmRuntimeGap::WorkloadCredential.detail()),
            IntegrationError::CredentialMissing,
            IntegrationError::NoFirm,
            IntegrationError::NoSuchProject,
            IntegrationError::CredentialUnusable,
            IntegrationError::Provider,
        ] {
            assert!(!error.slug().is_empty());
            let rendered = error.to_string();
            assert!(!rendered.contains("token"), "{rendered}");
            assert!(!rendered.contains("ciphertext"), "{rendered}");
        }
    }
}

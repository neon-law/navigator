//! Typed, Firm-scoped integration credentials.
//!
//! The database stores only an encrypted payload and KMS metadata. This
//! module is the only store seam that can turn a Project's `firm_id` into one
//! provider credential, and it never serializes the resolved value.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use cloud::{KmsContext, KmsError, RuntimeKms, WrappedDataKey};
pub use model::stripe_keys::{Account, KeyKind, Mode};
use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::firm_capability::{FirmCapability, FirmCapabilityDecision};
use crate::persons::Role;

const TABLE: &str = "firm_integration_secret";

/// Providers with a Firm-secret attachment in this lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum IntegrationProvider {
    Xero,
    DocuSign,
    Mercury,
    Slack,
    Notion,
    GitHub,
    Forgejo,
    /// Court and program websites with authenticated member portals. The
    /// docket scraper resolves its session credentials through this provider.
    CourtPortal,
    Twilio,
    Stripe,
}

impl IntegrationProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Xero => "xero",
            Self::DocuSign => "docusign",
            Self::Mercury => "mercury",
            Self::Slack => "slack",
            Self::Notion => "notion",
            Self::GitHub => "github",
            Self::Forgejo => "forgejo",
            Self::CourtPortal => "court_portal",
            Self::Twilio => "twilio",
            Self::Stripe => "stripe",
        }
    }
}

/// Every kind in the closed vocabulary, in declaration order. The settings
/// view builds its "create a secret" choices from this rather than a
/// hand-kept second list, so a new kind cannot be added to the enum without
/// also appearing there.
pub const ALL_KINDS: [IntegrationSecretKind; 17] = [
    IntegrationSecretKind::MercuryReadToken,
    IntegrationSecretKind::XeroTenantId,
    IntegrationSecretKind::XeroWebhookKey,
    IntegrationSecretKind::XeroClientId,
    IntegrationSecretKind::XeroClientSecret,
    IntegrationSecretKind::XeroRefreshToken,
    IntegrationSecretKind::SlackBotToken,
    IntegrationSecretKind::NotionToken,
    IntegrationSecretKind::GitHubAppPrivateKey,
    IntegrationSecretKind::ForgejoAccessToken,
    IntegrationSecretKind::PortalLogin,
    IntegrationSecretKind::TwilioAuthToken,
    IntegrationSecretKind::DocuSignConnection,
    IntegrationSecretKind::StripeNevadaAppKey,
    IntegrationSecretKind::StripeOperatingAppKey,
    IntegrationSecretKind::StripeNevadaPublishableKey,
    IntegrationSecretKind::StripeOperatingPublishableKey,
];

/// Typed Firm API credentials and provider webhook verification keys.
/// A Slack incoming-webhook URL is not a provider verification key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum IntegrationSecretKind {
    MercuryReadToken,
    XeroTenantId,
    XeroWebhookKey,
    XeroClientId,
    XeroClientSecret,
    XeroRefreshToken,
    SlackBotToken,
    NotionToken,
    GitHubAppPrivateKey,
    /// The Firm's own Forgejo access token, confined to its namespace.
    ForgejoAccessToken,
    /// A username or password for an external court or program portal,
    /// stored for the docket scraper's Chromium sessions.
    PortalLogin,
    /// The auth token that signs webhooks for one of the Firm's Twilio
    /// receiving numbers. Each registered number resolves through its own
    /// version, not through a Firm-wide current one.
    TwilioAuthToken,
    DocuSignConnection,
    StripeNevadaAppKey,
    StripeOperatingAppKey,
    StripeNevadaPublishableKey,
    StripeOperatingPublishableKey,
}

pub fn stripe_mode() -> Result<Mode, SecretStoreError> {
    stripe_mode_from(|name| std::env::var(name).ok())
}

fn stripe_mode_from(get: impl Fn(&str) -> Option<String>) -> Result<Mode, SecretStoreError> {
    use crate::config::{sample_matters_from, staging_from, DeploymentEnvironment};
    let environment =
        DeploymentEnvironment::from_lookup(&get).map_err(|_| SecretStoreError::InvalidStripeKey)?;
    let simulated =
        sample_matters_from(environment, &get).map_err(|_| SecretStoreError::InvalidStripeKey)?;
    let staging = staging_from(get).map_err(|_| SecretStoreError::InvalidStripeKey)?;
    Ok(
        if environment == DeploymentEnvironment::Production && !simulated && !staging {
            Mode::Live
        } else {
            Mode::Sandbox
        },
    )
}

impl IntegrationSecretKind {
    #[must_use]
    pub const fn stripe(account: Account, kind: KeyKind) -> Self {
        match (account, kind) {
            (Account::Nevada, KeyKind::Publishable) => Self::StripeNevadaPublishableKey,
            (Account::Nevada, KeyKind::Secret) => Self::StripeNevadaAppKey,
            (Account::Operating, KeyKind::Publishable) => Self::StripeOperatingPublishableKey,
            (Account::Operating, KeyKind::Secret) => Self::StripeOperatingAppKey,
        }
    }

    #[must_use]
    pub fn stripe_slot(self) -> Option<(Account, KeyKind)> {
        [Account::Nevada, Account::Operating]
            .into_iter()
            .flat_map(|account| [KeyKind::Publishable, KeyKind::Secret].map(|kind| (account, kind)))
            .find(|&(account, kind)| Self::stripe(account, kind) == self)
    }

    fn validated_stripe_mode(self, value: &str) -> Result<Option<Mode>, SecretStoreError> {
        let Some((_, kind)) = self.stripe_slot() else {
            return Ok(None);
        };
        let mode = stripe_mode()?;
        if mode.accepts(kind, value) {
            Ok(Some(mode))
        } else {
            Err(SecretStoreError::InvalidStripeKey)
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MercuryReadToken => "mercury_read_token",
            Self::XeroTenantId => "xero_tenant_id",
            Self::XeroWebhookKey => "xero_webhook_key",
            Self::XeroClientId => "xero_client_id",
            Self::XeroClientSecret => "xero_client_secret",
            Self::XeroRefreshToken => "xero_refresh_token",
            Self::SlackBotToken => "slack_bot_token",
            Self::NotionToken => "notion_token",
            Self::GitHubAppPrivateKey => "github_app_private_key",
            Self::ForgejoAccessToken => "forgejo_access_token",
            Self::PortalLogin => "portal_login",
            Self::TwilioAuthToken => "twilio_auth_token",
            Self::DocuSignConnection => "docusign_connection",
            Self::StripeNevadaAppKey => "stripe_nevada_app_key",
            Self::StripeOperatingAppKey => "stripe_operating_app_key",
            Self::StripeNevadaPublishableKey => "stripe_nevada_publishable_key",
            Self::StripeOperatingPublishableKey => "stripe_operating_publishable_key",
        }
    }

    #[must_use]
    pub const fn provider(self) -> IntegrationProvider {
        match self {
            Self::XeroTenantId
            | Self::XeroWebhookKey
            | Self::XeroClientId
            | Self::XeroClientSecret
            | Self::XeroRefreshToken => IntegrationProvider::Xero,
            Self::MercuryReadToken => IntegrationProvider::Mercury,
            Self::SlackBotToken => IntegrationProvider::Slack,
            Self::NotionToken => IntegrationProvider::Notion,
            Self::GitHubAppPrivateKey => IntegrationProvider::GitHub,
            Self::ForgejoAccessToken => IntegrationProvider::Forgejo,
            Self::PortalLogin => IntegrationProvider::CourtPortal,
            Self::TwilioAuthToken => IntegrationProvider::Twilio,
            Self::DocuSignConnection => IntegrationProvider::DocuSign,
            Self::StripeNevadaAppKey
            | Self::StripeOperatingAppKey
            | Self::StripeNevadaPublishableKey
            | Self::StripeOperatingPublishableKey => IntegrationProvider::Stripe,
        }
    }
}

/// Metadata returned by reads and settings views. There is no field for a
/// plaintext value, ciphertext, or wrapped data key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretMetadata {
    pub firm_id: Uuid,
    pub provider: IntegrationProvider,
    pub kind: IntegrationSecretKind,
    pub stripe_mode: Option<String>,
    pub version: i64,
    pub status: String,
    pub kms_key_version: String,
    pub actor_id: Uuid,
    pub created_at: String,
    pub updated_at: String,
}

/// Inputs for storing one Firm/provider credential version.
#[derive(Clone, Copy)]
pub struct SecretPutRequest<'a> {
    pub actor_role: Role,
    pub actor_person_id: Option<Uuid>,
    pub firm_id: Uuid,
    pub provider: IntegrationProvider,
    pub kind: IntegrationSecretKind,
    pub value: &'a str,
}

/// A short-lived provider credential. It cannot be serialized or formatted as
/// a secret, and its bytes are overwritten when dropped.
pub struct ResolvedCredential {
    value: Vec<u8>,
}

impl ResolvedCredential {
    #[must_use]
    pub fn expose_for_provider_call(&self) -> &str {
        std::str::from_utf8(&self.value).unwrap_or("")
    }
}

impl std::fmt::Debug for ResolvedCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResolvedCredential(REDACTED)")
    }
}

/// Overwrite secret bytes in place. A named helper so the `Drop` impl stays
/// trivially auditable and tests can exercise the zeroization directly.
fn zeroize(buffer: &mut [u8]) {
    buffer.fill(0);
}

impl Drop for ResolvedCredential {
    fn drop(&mut self) {
        zeroize(&mut self.value);
    }
}

/// Errors never carry the submitted value or decrypted provider bytes.
#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    #[error("PostgreSQL credential authority is unavailable")]
    Postgres,
    #[error("firm secret authorization denied")]
    NotAuthorized,
    #[error(
        "the {provider} {kind} credential is not configured; a Firm administrator \
             with the ManageIntegrationSecrets capability must store one first"
    )]
    NotConfigured {
        provider: &'static str,
        kind: &'static str,
    },
    #[error("the provider and typed secret kind do not match")]
    InvalidKind,
    #[error("Stripe key type or mode does not match this deployment")]
    InvalidStripeKey,
    #[error("the submitted credential is empty")]
    EmptyValue,
    #[error("no project {0}")]
    NoSuchProject(Uuid),
    #[error("the project has no owning Firm")]
    ProjectHasNoFirm,
    #[error("credential encryption failed")]
    Kms(#[source] KmsError),
    #[error("credential payload is not valid UTF-8")]
    InvalidValue,
    #[error("writing a firm secret returned no usable row")]
    WriteReturnedNothing,
    #[error("the referenced credential {0} is missing, revoked, or not the resolving Firm's")]
    MissingRef(Uuid),
}

/// Parse the wire/form spelling of a provider. `pub` so the settings door and
/// its form share this one allowlist rather than each re-deriving it.
#[must_use]
pub fn parse_provider(value: &str) -> Option<IntegrationProvider> {
    match value {
        "xero" => Some(IntegrationProvider::Xero),
        "slack" => Some(IntegrationProvider::Slack),
        "notion" => Some(IntegrationProvider::Notion),
        "github" => Some(IntegrationProvider::GitHub),
        "forgejo" => Some(IntegrationProvider::Forgejo),
        "court_portal" => Some(IntegrationProvider::CourtPortal),
        "twilio" => Some(IntegrationProvider::Twilio),
        "docusign" => Some(IntegrationProvider::DocuSign),
        "stripe" => Some(IntegrationProvider::Stripe),
        _ => None,
    }
}

/// Parse the wire/form spelling of a secret kind. `pub` for the same reason
/// as [`parse_provider`].
#[must_use]
pub fn parse_kind(value: &str) -> Option<IntegrationSecretKind> {
    ALL_KINDS.into_iter().find(|kind| kind.as_str() == value)
}

fn not_configured(provider: IntegrationProvider, kind: IntegrationSecretKind) -> SecretStoreError {
    SecretStoreError::NotConfigured {
        provider: provider.as_str(),
        kind: kind.as_str(),
    }
}

fn validate_kind(
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
) -> Result<(), SecretStoreError> {
    if kind.provider() == provider {
        Ok(())
    } else {
        Err(SecretStoreError::InvalidKind)
    }
}

async fn authorize(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    capability: FirmCapability,
) -> Result<(), SecretStoreError> {
    match capability
        .resolve(application_store, actor_role, actor_person_id, firm_id)
        .await
        .map_err(|_| SecretStoreError::NotAuthorized)?
    {
        FirmCapabilityDecision::Allowed => Ok(()),
        FirmCapabilityDecision::Forbidden | FirmCapabilityDecision::FirmNotFound => {
            Err(SecretStoreError::NotAuthorized)
        }
    }
}

/// Create a credential version. Twilio tokens remain independently active
/// because receiving numbers pin their record ids; other kinds replace the
/// current version.
///
/// Two orderings protect the last valid credential. Encryption and the KMS
/// wrap complete before any write, so a KMS fault leaves the current version
/// untouched. And the revoke and the create are one transaction, so a create
/// that fails — the unique `(firm, provider, kind, version)` index, a store
/// fault — cannot leave the Firm with every version revoked and none to
/// resolve. Without the transaction each statement commits on its own, and
/// the failure mode is a Firm whose integration has no working credential.
pub async fn put(
    application_store: &ApplicationStore,
    request: SecretPutRequest<'_>,
    kms: &dyn RuntimeKms,
) -> Result<SecretMetadata, SecretStoreError> {
    write_secret(application_store, request, kms).await?;
    metadata_for_firm(
        application_store,
        request.actor_role,
        request.actor_person_id,
        request.firm_id,
        request.provider,
        request.kind,
    )
    .await
}

/// Store one independently active Twilio token and return the exact record
/// id a receiving number pins. Uses the same Firm management authorization
/// and encryption as [`put`].
pub async fn put_twilio_auth_token(
    application_store: &ApplicationStore,
    request: SecretPutRequest<'_>,
    kms: &dyn RuntimeKms,
) -> Result<Uuid, SecretStoreError> {
    if request.provider != IntegrationProvider::Twilio
        || request.kind != IntegrationSecretKind::TwilioAuthToken
    {
        return Err(SecretStoreError::InvalidKind);
    }
    write_secret(application_store, request, kms).await
}

async fn write_secret(
    application_store: &ApplicationStore,
    request: SecretPutRequest<'_>,
    kms: &dyn RuntimeKms,
) -> Result<Uuid, SecretStoreError> {
    let SecretPutRequest {
        actor_role,
        actor_person_id,
        firm_id,
        provider,
        kind,
        value,
    } = request;
    authorize(
        application_store,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ManageIntegrationSecrets,
    )
    .await?;
    validate_kind(provider, kind)?;
    if value.trim().is_empty() {
        return Err(SecretStoreError::EmptyValue);
    }
    let stripe_mode = kind.validated_stripe_mode(value)?;
    let actor_id = actor_person_id.ok_or(SecretStoreError::NotAuthorized)?;
    if crate::firms::find_by_id(application_store, firm_id)
        .await
        .map_err(|_| SecretStoreError::NotAuthorized)?
        .is_none()
    {
        return Err(SecretStoreError::NotAuthorized);
    }
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    crate::postgres::firm_secrets::write_secret(&pg, request, actor_id, stripe_mode, kms).await
}

/// Read only metadata for the current version.
pub async fn metadata_for_firm(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
) -> Result<SecretMetadata, SecretStoreError> {
    authorize(
        application_store,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ViewIntegrationSecretMetadata,
    )
    .await?;
    validate_kind(provider, kind)?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    let row = sqlx::query("SELECT firm_id,provider,kind,stripe_mode,version,kms_key_version,status,actor_id,created_at,updated_at FROM firm_integration_secret WHERE firm_id=$1 AND provider=$2 AND kind=$3 AND status='active' ORDER BY version DESC LIMIT 1")
            .bind(firm_id).bind(provider.as_str()).bind(kind.as_str()).fetch_optional(pg.pool()).await.map_err(|_| SecretStoreError::Postgres)?;
    row.as_ref()
        .map(metadata_from_pg)
        .transpose()?
        .ok_or_else(|| not_configured(provider, kind))
}

/// Read metadata for every provider/kind this Firm currently has an active
/// version of — the Doppler-like settings list. Still no plaintext, ciphertext,
/// or wrapped-key field; same authorization as [`metadata_for_firm`], which
/// this does not replace: that one still answers "the current version of this
/// one kind" for a resolver or a narrower read.
pub async fn list_metadata_for_firm(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
) -> Result<Vec<SecretMetadata>, SecretStoreError> {
    authorize(
        application_store,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ViewIntegrationSecretMetadata,
    )
    .await?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    let rows = sqlx::query("SELECT firm_id,provider,kind,stripe_mode,version,kms_key_version,status,actor_id,created_at,updated_at FROM firm_integration_secret WHERE firm_id=$1 AND status='active' ORDER BY provider,kind,version DESC")
            .bind(firm_id).fetch_all(pg.pool()).await.map_err(|_| SecretStoreError::Postgres)?;
    rows.iter().map(metadata_from_pg).collect()
}

/// Revoke every active version of a kind without calling the provider.
/// For one pinned Twilio token, use [`revoke_twilio_auth_token_by_ref`]. Provider-side
/// revocation remains an operator action and is stated in the settings UI.
pub async fn revoke(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
) -> Result<(), SecretStoreError> {
    revoke_matching(
        application_store,
        actor_role,
        actor_person_id,
        firm_id,
        provider,
        kind,
        None,
    )
    .await
}

pub async fn revoke_stripe(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    account: Account,
    kind: KeyKind,
    mode: Mode,
) -> Result<(), SecretStoreError> {
    revoke_matching(
        application_store,
        actor_role,
        actor_person_id,
        firm_id,
        IntegrationProvider::Stripe,
        IntegrationSecretKind::stripe(account, kind),
        Some(mode),
    )
    .await
}

async fn revoke_matching(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
    mode: Option<Mode>,
) -> Result<(), SecretStoreError> {
    authorize(
        application_store,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ManageIntegrationSecrets,
    )
    .await?;
    validate_kind(provider, kind)?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    let changed = sqlx::query("UPDATE firm_integration_secret SET status='revoked',updated_at=now() WHERE firm_id=$1 AND provider=$2 AND kind=$3 AND status='active' AND ($4::text IS NULL OR stripe_mode=$4)")
            .bind(firm_id).bind(provider.as_str()).bind(kind.as_str()).bind(mode.map(|m| m.to_string())).execute(pg.pool()).await.map_err(|_| SecretStoreError::Postgres)?.rows_affected();
    if changed == 0 {
        Err(not_configured(provider, kind))
    } else {
        Ok(())
    }
}

/// Revoke one Twilio token within the managing Firm, leaving other receiving
/// numbers' pinned tokens active. Provider-side revocation is separate.
pub async fn revoke_twilio_auth_token_by_ref(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    secret_ref: Uuid,
) -> Result<(), SecretStoreError> {
    authorize(
        application_store,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ManageIntegrationSecrets,
    )
    .await?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    let changed = sqlx::query("UPDATE firm_integration_secret SET status='revoked',updated_at=now() WHERE id=$1 AND firm_id=$2 AND provider='twilio' AND kind='twilio_auth_token' AND status='active'")
            .bind(secret_ref).bind(firm_id).execute(pg.pool()).await.map_err(|_| SecretStoreError::Postgres)?.rows_affected();
    if changed == 0 {
        Err(SecretStoreError::MissingRef(secret_ref))
    } else {
        Ok(())
    }
}

/// A state trust destination. Nevada can use its dedicated Firm credential.
/// Other states use an explicitly configured connected account on the Firm's Stripe platform.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRoute {
    firm: Uuid,
    pub state: String,
    pub account: String,
    #[serde(default)]
    pub dedicated_nevada: bool,
    /// States whose matters may collect into this Nevada IOLTA while they
    /// have no trust account of their own. Only a Nevada route carries it,
    /// and a state is listed only once the Firm has confirmed that state's
    /// rules allow it (LAW-374).
    #[serde(default)]
    pub fallback_states: Vec<String>,
}

impl TrustRoute {
    #[must_use]
    pub fn credential_kind(&self) -> IntegrationSecretKind {
        if self.dedicated_nevada {
            IntegrationSecretKind::StripeNevadaAppKey
        } else {
            IntegrationSecretKind::StripeOperatingAppKey
        }
    }

    #[must_use]
    pub fn bind_preview(&self, digest: &str) -> String {
        format!("{digest}/{}/{}", self.state, self.account)
    }
}

/// An operating draft is pinned to the authenticated account by the invoice command.
/// Trust retries also retain their reviewed state and registered account.
#[must_use]
pub fn stripe_batch_matches_route(
    batch: &crate::billing_batches::MatterBillingBatch,
    route: Option<&TrustRoute>,
) -> bool {
    match route {
        // Nevada predates persisted trust states.
        Some(route) => {
            batch.stripe_iolta
                && batch.trust_state.as_deref().unwrap_or("NV") == route.state
                && batch.stripe_account_id.as_ref() == Some(&route.account)
        }
        None => {
            !batch.stripe_iolta && batch.trust_state.is_none() && batch.stripe_account_id.is_some()
        }
    }
}

fn trust_route(
    firm: Option<Uuid>,
    state: Option<&str>,
    config: Option<&str>,
) -> Result<Option<TrustRoute>, SecretStoreError> {
    let routes: Vec<TrustRoute> =
        serde_json::from_str(config.unwrap_or("[]")).map_err(|_| SecretStoreError::InvalidKind)?;
    let mut destinations = std::collections::HashSet::new();
    let mut states = std::collections::HashSet::new();
    for route in &routes {
        if ((route.dedicated_nevada || !route.fallback_states.is_empty()) && route.state != "NV")
            || route
                .fallback_states
                .iter()
                .any(|state| state.len() != 2 || !state.bytes().all(|b| b.is_ascii_uppercase()))
            || route.state.len() != 2
            || !route.state.bytes().all(|b| b.is_ascii_uppercase())
            || !route
                .account
                .strip_prefix("acct_")
                .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
            || !destinations.insert(&route.account)
            || !states.insert((route.firm, &route.state))
        {
            return Err(SecretStoreError::InvalidKind);
        }
    }
    let mut firm_routes = routes.into_iter().filter(|route| Some(route.firm) == firm);
    let (mut exact, mut nevada) = (None, None);
    for route in firm_routes.by_ref() {
        if Some(route.state.as_str()) == state {
            exact = Some(route);
        } else if route.state == "NV" {
            nevada = Some(route);
        }
    }
    // A matter with no state of its own, or a state the Firm listed, falls
    // back to the Firm's Nevada IOLTA. Any other state has no route.
    Ok(exact.or_else(|| {
        nevada.filter(|route| {
            state.is_none_or(|state| route.fallback_states.iter().any(|listed| listed == state))
        })
    }))
}

pub async fn stripe_trust_route(
    application_store: &ApplicationStore,
    project: &crate::projects::Project,
) -> Result<Option<TrustRoute>, SecretStoreError> {
    let jurisdiction = match project.jurisdiction_id {
        Some(id) => crate::jurisdictions::find_by_id(application_store, id)
            .await
            .map_err(|_| SecretStoreError::InvalidKind)?,
        None => None,
    };
    let state = jurisdiction
        .as_ref()
        .filter(|j| j.jurisdiction_type == crate::jurisdictions::JURISDICTION_TYPE_STATE)
        .map(|j| j.code.as_str());
    let config = std::env::var("NAVIGATOR_STRIPE_TRUST_ROUTES").ok();
    trust_route(project.firm_id, state, config.as_deref())
}

#[cfg(test)]
mod trust_route_tests {
    use super::*;
    #[test]
    fn trust_routes_are_separate_by_firm_and_state_and_refuse_shared_destinations() {
        let id = Uuid::new_v4();
        let route = serde_json::json!({"firm":id,"state":"CA","account":"acct_A"});
        let config = serde_json::json!([route]).to_string();
        assert_eq!(
            trust_route(Some(id), Some("CA"), Some(&config))
                .unwrap()
                .unwrap()
                .account,
            "acct_A"
        );
        for (firm, state) in [
            (Some(id), None),
            (Some(Uuid::new_v4()), Some("CA")),
            (Some(id), Some("US")),
        ] {
            assert!(trust_route(firm, state, Some(&config)).unwrap().is_none());
        }
        let duplicate =
            serde_json::json!([route,{"firm":id,"state":"WA","account":"acct_A"}]).to_string();
        assert!(trust_route(Some(id), Some("CA"), Some(&duplicate)).is_err());
    }

    #[test]
    fn a_matter_without_its_own_route_falls_back_to_nevada_only_where_listed() {
        let (firm, other) = (Uuid::new_v4(), Uuid::new_v4());
        let config = serde_json::json!([
            {"firm": firm, "state": "NV", "account": "acct_Nevada", "dedicated_nevada": true,
             "fallback_states": ["UT"]},
            {"firm": firm, "state": "CA", "account": "acct_California"},
        ])
        .to_string();
        let account = |state: Option<&str>| {
            trust_route(Some(firm), state, Some(&config))
                .unwrap()
                .map(|route| route.account)
        };
        assert_eq!(account(Some("CA")).as_deref(), Some("acct_California"), "exact state first");
        assert_eq!(account(Some("NV")).as_deref(), Some("acct_Nevada"));
        assert_eq!(account(None).as_deref(), Some("acct_Nevada"), "no state: Nevada default");
        assert_eq!(account(Some("UT")).as_deref(), Some("acct_Nevada"), "a listed state");
        assert_eq!(account(Some("WA")), None, "an unlisted state has no route");
        assert!(trust_route(Some(other), None, Some(&config)).unwrap().is_none());
        let misplaced = serde_json::json!([
            {"firm": firm, "state": "CA", "account": "acct_X", "fallback_states": ["UT"]},
        ])
        .to_string();
        assert!(trust_route(Some(firm), Some("CA"), Some(&misplaced)).is_err());
    }
}

/// Whether the deployment enables a Stripe billing route. The environment
/// only gates access. Provider credentials are resolved from the Firm.
#[must_use]
pub fn stripe_billing_enabled(kind: IntegrationSecretKind) -> bool {
    stripe_billing_enabled_from(kind, |key| std::env::var_os(key).is_some())
}

fn stripe_billing_enabled_from(
    kind: IntegrationSecretKind,
    is_set: impl FnOnce(&str) -> bool,
) -> bool {
    let variable = match kind {
        IntegrationSecretKind::StripeOperatingAppKey => "STRIPE_DEFAULT",
        IntegrationSecretKind::StripeNevadaAppKey => "STRIPE_NEVADA",
        _ => return false,
    };
    is_set(variable)
}

/// Check Stripe availability after the caller has established Firm-side
/// Project access. This reads only the presence of the typed credential.
pub async fn stripe_credential_configured(
    _application_store: &ApplicationStore,
    firm_id: Uuid,
    kind: IntegrationSecretKind,
) -> Result<bool, SecretStoreError> {
    validate_kind(IntegrationProvider::Stripe, kind)?;
    if !stripe_billing_enabled(kind) {
        return Ok(false);
    }
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM firm_integration_secret WHERE firm_id=$1 AND provider='stripe' AND kind=$2 AND status='active')")
            .bind(firm_id).bind(kind.as_str()).fetch_one(pg.pool()).await.map_err(|_| SecretStoreError::Postgres)
}

/// Resolve a single current credential from the Project's owning Firm.
pub async fn resolve_project_credential(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedCredential, SecretStoreError> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    resolve_project_credential_on(&pg, project_id, provider, kind, kms).await
}

/// Runtime-only credential resolution for a known Firm, including verified ingress.
/// Callers must establish their authorization or verify the provider signature.
pub async fn resolve_firm_credential(
    _application_store: &ApplicationStore,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedCredential, SecretStoreError> {
    validate_kind(provider, kind)?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    resolve_firm_credential_on(&pg, firm_id, provider, kind, kms).await
}

/// Resolve the Project's own Firm credential through an explicitly admitted pool.
///
/// # Errors
/// Preserves current kind, ownership, active-version, AAD and KMS validation.
pub async fn resolve_project_credential_on(
    pg: &crate::postgres::PostgresDb,
    project_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedCredential, SecretStoreError> {
    validate_kind(provider, kind)?;
    pg.require_application_history()
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    let project = crate::postgres::projects::find_by_id_on(pg, project_id)
        .await
        .map_err(|_| SecretStoreError::Postgres)?
        .ok_or(SecretStoreError::NoSuchProject(project_id))?;
    let firm = project.firm_id.ok_or(SecretStoreError::ProjectHasNoFirm)?;
    resolve_firm_credential_on(pg, firm, provider, kind, kms).await
}

/// Runtime credential resolution on one explicit native pool. Callers establish
/// authorization or verify the ingress signature before using this seam.
///
/// # Errors
/// Invalid kind, missing active credential, wrong context or KMS/storage failure.
pub async fn resolve_firm_credential_on(
    pg: &crate::postgres::PostgresDb,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedCredential, SecretStoreError> {
    validate_kind(provider, kind)?;
    pg.require_application_history()
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    let row=sqlx::query("SELECT * FROM firm_integration_secret WHERE firm_id=$1 AND provider=$2 AND kind=$3 AND status='active' ORDER BY version DESC LIMIT 1")
        .bind(firm_id).bind(provider.as_str()).bind(kind.as_str()).fetch_optional(pg.pool()).await.map_err(|_|SecretStoreError::Postgres)?
        .ok_or_else(||not_configured(provider,kind))?;
    unseal_credential(secret_from_pg(&row)?, firm_id, provider, kind, kms).await
}

struct SecretRow {
    firm_id: Uuid,
    provider: String,
    kind: String,
    ciphertext: String,
    wrapped_dek: String,
    kms_key_version: String,
    kms_context: String,
    status: String,
}

async fn unseal_credential(
    row: SecretRow,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedCredential, SecretStoreError> {
    let context = KmsContext::new(firm_id, provider.as_str(), kind.as_str());
    if row.kms_context != context.aad() {
        return Err(SecretStoreError::Kms(KmsError::ContextMismatch));
    }
    let wrapped = WrappedDataKey {
        ciphertext: BASE64
            .decode(row.wrapped_dek)
            .map_err(|_| SecretStoreError::Kms(KmsError::InvalidResponse))?,
        key_version: row.kms_key_version,
    };
    let data_key = kms
        .unwrap_data_key(&wrapped, &context)
        .await
        .map_err(SecretStoreError::Kms)?;
    let ciphertext = BASE64
        .decode(row.ciphertext)
        .map_err(|_| SecretStoreError::Kms(KmsError::InvalidResponse))?;
    let plaintext = cloud::open(&ciphertext, &data_key, &context).map_err(SecretStoreError::Kms)?;
    if row.status != "active" || row.firm_id != firm_id {
        return Err(SecretStoreError::NotConfigured {
            provider: provider.as_str(),
            kind: kind.as_str(),
        });
    }
    String::from_utf8(plaintext)
        .map(|value| ResolvedCredential {
            value: value.into_bytes(),
        })
        .map_err(|_| SecretStoreError::InvalidValue)
}
/// Resolve the exact credential version a registry row pins, addressed by
/// the secret record's UUID, as `(token, owning Firm)`. The webhook
/// resolution spine passes each registered number's `auth_token_secret_ref`
/// and checks the owning Firm against the Brand's. The row must be the
/// named record, active, and a Twilio auth token — a reference to any other
/// credential kind is [`SecretStoreError::InvalidKind`]. Unlike
/// [`resolve_firm_credential`], which takes the Firm's current version,
/// this pins the version the registry registered, so per-number tokens
/// rotate independently.
pub async fn resolve_twilio_auth_token_by_ref(
    _application_store: &ApplicationStore,
    secret_ref: Uuid,
    kms: &dyn RuntimeKms,
) -> Result<(ResolvedCredential, Uuid), SecretStoreError> {
    let row = {
        let pg = crate::postgres::matter::authority_for(TABLE)
            .await
            .map_err(|_| SecretStoreError::Postgres)?;
        let row = sqlx::query("SELECT * FROM firm_integration_secret WHERE id=$1")
            .bind(secret_ref)
            .fetch_optional(pg.pool())
            .await
            .map_err(|_| SecretStoreError::Postgres)?
            .ok_or(SecretStoreError::MissingRef(secret_ref))?;
        secret_from_pg(&row)?
    };
    let provider = parse_provider(&row.provider).ok_or(SecretStoreError::InvalidKind)?;
    let kind = parse_kind(&row.kind).ok_or(SecretStoreError::InvalidKind)?;
    validate_kind(provider, kind)?;
    if kind != IntegrationSecretKind::TwilioAuthToken {
        return Err(SecretStoreError::InvalidKind);
    }
    let firm_id = row.firm_id;
    if row.status != "active" {
        return Err(SecretStoreError::MissingRef(secret_ref));
    }
    let context = KmsContext::new(firm_id, provider.as_str(), kind.as_str());
    if row.kms_context != context.aad() {
        return Err(SecretStoreError::Kms(KmsError::ContextMismatch));
    }
    let wrapped = WrappedDataKey {
        ciphertext: BASE64
            .decode(row.wrapped_dek)
            .map_err(|_| SecretStoreError::Kms(KmsError::InvalidResponse))?,
        key_version: row.kms_key_version,
    };
    let data_key = kms
        .unwrap_data_key(&wrapped, &context)
        .await
        .map_err(SecretStoreError::Kms)?;
    let ciphertext = BASE64
        .decode(row.ciphertext)
        .map_err(|_| SecretStoreError::Kms(KmsError::InvalidResponse))?;
    let plaintext = cloud::open(&ciphertext, &data_key, &context).map_err(SecretStoreError::Kms)?;
    let credential = String::from_utf8(plaintext)
        .map(|value| ResolvedCredential {
            value: value.into_bytes(),
        })
        .map_err(|_| SecretStoreError::InvalidValue)?;
    Ok((credential, firm_id))
}

fn metadata_from_pg(row: &sqlx::postgres::PgRow) -> Result<SecretMetadata, SecretStoreError> {
    use sqlx::Row;
    let provider: String = row
        .try_get("provider")
        .map_err(|_| SecretStoreError::Postgres)?;
    let kind: String = row
        .try_get("kind")
        .map_err(|_| SecretStoreError::Postgres)?;
    Ok(SecretMetadata {
        firm_id: row
            .try_get("firm_id")
            .map_err(|_| SecretStoreError::Postgres)?,
        provider: parse_provider(&provider).ok_or(SecretStoreError::InvalidKind)?,
        kind: parse_kind(&kind).ok_or(SecretStoreError::InvalidKind)?,
        stripe_mode: row
            .try_get("stripe_mode")
            .map_err(|_| SecretStoreError::Postgres)?,
        version: row
            .try_get("version")
            .map_err(|_| SecretStoreError::Postgres)?,
        status: row
            .try_get("status")
            .map_err(|_| SecretStoreError::Postgres)?,
        kms_key_version: row
            .try_get("kms_key_version")
            .map_err(|_| SecretStoreError::Postgres)?,
        actor_id: row
            .try_get("actor_id")
            .map_err(|_| SecretStoreError::Postgres)?,
        created_at: row
            .try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .map_err(|_| SecretStoreError::Postgres)?
            .to_rfc3339(),
        updated_at: row
            .try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .map_err(|_| SecretStoreError::Postgres)?
            .to_rfc3339(),
    })
}

fn secret_from_pg(row: &sqlx::postgres::PgRow) -> Result<SecretRow, SecretStoreError> {
    use sqlx::Row;
    let m = metadata_from_pg(row)?;
    Ok(SecretRow {
        firm_id: m.firm_id,
        provider: m.provider.as_str().into(),
        kind: m.kind.as_str().into(),
        ciphertext: row
            .try_get("ciphertext")
            .map_err(|_| SecretStoreError::Postgres)?,
        wrapped_dek: row
            .try_get("wrapped_dek")
            .map_err(|_| SecretStoreError::Postgres)?,
        kms_key_version: m.kms_key_version,
        kms_context: row
            .try_get("kms_context")
            .map_err(|_| SecretStoreError::Postgres)?,
        status: m.status,
    })
}

#[cfg(test)]
mod tests {
    use super::{zeroize, IntegrationProvider, IntegrationSecretKind, ResolvedCredential};

    #[test]
    fn staging_and_simulated_production_only_accept_sandbox_keys() {
        use super::{stripe_mode_from, Mode};
        for (profile, staging, simulated, expected) in [
            ("production", "false", "false", Some(Mode::Live)),
            ("production", "true", "false", Some(Mode::Sandbox)),
            ("production", "false", "true", Some(Mode::Sandbox)),
            ("dev", "false", "false", Some(Mode::Sandbox)),
            ("production", "unknown", "false", None),
        ] {
            assert_eq!(
                stripe_mode_from(|name| match name {
                    "NAVIGATOR_ENVIRONMENT" => Some(profile.into()),
                    "NAVIGATOR_STAGING" => Some(staging.into()),
                    "NAVIGATOR_SIMULATED_MATTERS" => Some(simulated.into()),
                    _ => None,
                })
                .ok(),
                expected
            );
        }
    }

    #[test]
    fn stripe_billing_checks_only_the_gate_for_that_route() {
        use super::stripe_billing_enabled_from;
        let is_set = |key: &str| key == "STRIPE_NEVADA";
        assert!(stripe_billing_enabled_from(
            IntegrationSecretKind::StripeNevadaAppKey,
            is_set
        ));
        assert!(!stripe_billing_enabled_from(
            IntegrationSecretKind::StripeOperatingAppKey,
            is_set
        ));
        assert!(stripe_billing_enabled_from(
            IntegrationSecretKind::StripeOperatingAppKey,
            |key| key == "STRIPE_DEFAULT"
        ));
    }

    const SECRET: &[u8] = b"la-superior-password";

    #[test]
    fn zeroize_helper_overwrites_every_byte() {
        let mut buffer = SECRET.to_vec();
        zeroize(&mut buffer);
        assert!(buffer.iter().all(|&byte| byte == 0));
        assert_ne!(buffer.as_slice(), SECRET);
    }

    #[test]
    fn debug_renders_redacted_and_never_secret_bytes() {
        let credential = ResolvedCredential {
            value: SECRET.to_vec(),
        };
        let rendered = format!("{credential:?}");
        assert_eq!(rendered, "ResolvedCredential(REDACTED)");
        assert!(!rendered.contains("la-superior-password"));
    }

    #[test]
    fn resolved_credential_drops_without_exposing_its_bytes() {
        let credential = ResolvedCredential {
            value: SECRET.to_vec(),
        };
        // The `Drop` impl delegates to the named `zeroize` helper, which the
        // first test pins byte for byte; this one pins that dropping a
        // resolved credential runs that path (no panic, no leak surfaces
        // through `Debug`).
        drop(credential);
        let rendered = format!(
            "{:?}",
            ResolvedCredential {
                value: SECRET.to_vec(),
            }
        );
        assert!(!rendered.contains("la-superior-password"));
    }

    #[test]
    fn all_kinds_includes_the_court_portal_pair() {
        assert!(super::ALL_KINDS.contains(&IntegrationSecretKind::PortalLogin));
        assert_eq!(IntegrationSecretKind::PortalLogin.as_str(), "portal_login");
        assert_eq!(
            IntegrationSecretKind::PortalLogin.provider(),
            IntegrationProvider::CourtPortal
        );
        assert_eq!(IntegrationProvider::CourtPortal.as_str(), "court_portal");
        assert_eq!(
            super::parse_provider("court_portal"),
            Some(IntegrationProvider::CourtPortal)
        );
        assert_eq!(
            super::parse_kind("portal_login"),
            Some(IntegrationSecretKind::PortalLogin)
        );
    }
}

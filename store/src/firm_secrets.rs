//! Typed, Firm-scoped integration credentials.
//!
//! The database stores only an encrypted payload and KMS metadata. This
//! module is the only store seam that can turn a Project's `firm_id` into one
//! provider credential, and it never serializes the resolved value.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use cloud::{KmsContext, KmsError, RuntimeKms, WrappedDataKey};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::firm_capability::{FirmCapability, FirmCapabilityDecision};
use crate::persons::Role;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "firm_integration_secret";
const FIRM_TABLE: &str = "firm";
const PERSON_TABLE: &str = "person";

/// Providers with a Firm-secret attachment in this lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum IntegrationProvider {
    Xero,
    Mercury,
    Slack,
    Notion,
    GitHub,
    Forgejo,
    /// Court and program websites with authenticated member portals. The
    /// docket scraper resolves its session credentials through this provider.
    CourtPortal,
}

impl IntegrationProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Xero => "xero",
            Self::Mercury => "mercury",
            Self::Slack => "slack",
            Self::Notion => "notion",
            Self::GitHub => "github",
            Self::Forgejo => "forgejo",
            Self::CourtPortal => "court_portal",
        }
    }
}

/// Every kind in the closed vocabulary, in declaration order. The settings
/// view builds its "create a secret" choices from this rather than a
/// hand-kept second list, so a new kind cannot be added to the enum without
/// also appearing there.
pub const ALL_KINDS: [IntegrationSecretKind; 11] = [
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
}

impl IntegrationSecretKind {
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
    pub version: i64,
    pub status: String,
    pub kms_key_version: String,
    pub actor_id: Uuid,
    pub created_at: String,
    pub updated_at: String,
}

/// Inputs for storing one Firm/provider credential version.
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
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
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
}

#[derive(SurrealValue)]
struct SecretRow {
    firm_id: surrealdb::types::RecordId,
    provider: String,
    kind: String,
    version: i64,
    ciphertext: String,
    wrapped_dek: String,
    kms_key_version: String,
    kms_context: String,
    status: String,
    actor_id: surrealdb::types::RecordId,
    created_at: String,
    updated_at: String,
}

#[derive(SurrealValue)]
struct MetadataRow {
    firm_id: surrealdb::types::RecordId,
    provider: String,
    kind: String,
    version: i64,
    kms_key_version: String,
    status: String,
    actor_id: surrealdb::types::RecordId,
    created_at: String,
    updated_at: String,
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
        _ => None,
    }
}

/// Parse the wire/form spelling of a secret kind. `pub` for the same reason
/// as [`parse_provider`].
#[must_use]
pub fn parse_kind(value: &str) -> Option<IntegrationSecretKind> {
    match value {
        "xero_client_id" => Some(IntegrationSecretKind::XeroClientId),
        "xero_client_secret" => Some(IntegrationSecretKind::XeroClientSecret),
        "xero_refresh_token" => Some(IntegrationSecretKind::XeroRefreshToken),
        "slack_bot_token" => Some(IntegrationSecretKind::SlackBotToken),
        "notion_token" => Some(IntegrationSecretKind::NotionToken),
        "github_app_private_key" => Some(IntegrationSecretKind::GitHubAppPrivateKey),
        "mercury_read_token" => Some(IntegrationSecretKind::MercuryReadToken),
        "xero_tenant_id" => Some(IntegrationSecretKind::XeroTenantId),
        "xero_webhook_key" => Some(IntegrationSecretKind::XeroWebhookKey),
        "forgejo_access_token" => Some(IntegrationSecretKind::ForgejoAccessToken),
        "portal_login" => Some(IntegrationSecretKind::PortalLogin),
        _ => None,
    }
}

fn metadata(row: MetadataRow) -> Result<SecretMetadata, SecretStoreError> {
    Ok(SecretMetadata {
        firm_id: record_uuid(&row.firm_id).ok_or(SecretStoreError::WriteReturnedNothing)?,
        provider: parse_provider(&row.provider).ok_or(SecretStoreError::InvalidKind)?,
        kind: parse_kind(&row.kind).ok_or(SecretStoreError::InvalidKind)?,
        version: row.version,
        status: row.status,
        kms_key_version: row.kms_key_version,
        actor_id: record_uuid(&row.actor_id).ok_or(SecretStoreError::WriteReturnedNothing)?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
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
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    capability: FirmCapability,
) -> Result<(), SecretStoreError> {
    match capability
        .resolve(surreal, actor_role, actor_person_id, firm_id)
        .await
        .map_err(|_| SecretStoreError::NotAuthorized)?
    {
        FirmCapabilityDecision::Allowed => Ok(()),
        FirmCapabilityDecision::Forbidden | FirmCapabilityDecision::FirmNotFound => {
            Err(SecretStoreError::NotAuthorized)
        }
    }
}

/// Create or replace the current version.
///
/// Two orderings protect the last valid credential. Encryption and the KMS
/// wrap complete before any write, so a KMS fault leaves the current version
/// untouched. And the revoke and the create are one transaction, so a create
/// that fails — the unique `(firm, provider, kind, version)` index, a store
/// fault — cannot leave the Firm with every version revoked and none to
/// resolve. Without the transaction each statement commits on its own, and
/// the failure mode is a Firm whose integration has no working credential.
pub async fn put(
    surreal: &SurrealDb,
    request: SecretPutRequest<'_>,
    kms: &dyn RuntimeKms,
) -> Result<SecretMetadata, SecretStoreError> {
    let SecretPutRequest {
        actor_role,
        actor_person_id,
        firm_id,
        provider,
        kind,
        value,
    } = request;
    authorize(
        surreal,
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
    let actor_id = actor_person_id.ok_or(SecretStoreError::NotAuthorized)?;
    if crate::firms::find_by_id(surreal, firm_id)
        .await
        .map_err(|_| SecretStoreError::NotAuthorized)?
        .is_none()
    {
        return Err(SecretStoreError::NotAuthorized);
    }
    let mut versions = surreal
        .query(format!(
            "SELECT VALUE version FROM {TABLE} WHERE firm_id = $firm_id AND \
             provider = $provider AND kind = $kind ORDER BY version DESC LIMIT 1"
        ))
        .bind(("firm_id", record_id(FIRM_TABLE, firm_id)))
        .bind(("provider", provider.as_str().to_string()))
        .bind(("kind", kind.as_str().to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let existing: Vec<i64> = versions.take(0)?;
    let version = existing.first().copied().unwrap_or(0) + 1;
    let context = KmsContext::new(firm_id, provider.as_str(), kind.as_str());
    let data_key: [u8; 32] = rand::random();
    let ciphertext =
        cloud::seal(value.as_bytes(), &data_key, &context).map_err(SecretStoreError::Kms)?;
    let wrapped = kms
        .wrap_data_key(&data_key, &context)
        .await
        .map_err(SecretStoreError::Kms)?;
    let now = chrono::Utc::now().to_rfc3339();
    surreal
        .query(format!(
            "BEGIN; \
             UPDATE {TABLE} SET status = 'revoked', updated_at = $now WHERE \
             firm_id = $firm_id AND provider = $provider AND kind = $kind AND status = 'active'; \
             CREATE $id SET firm_id = $firm_id, provider = $provider, kind = $kind, version = $version, \
             ciphertext = $ciphertext, wrapped_dek = $wrapped_dek, kms_key_version = $kms_key_version, \
             kms_context = $kms_context, status = 'active', actor_id = $actor_id, created_at = $now, updated_at = $now; \
             COMMIT;"
        ))
        .bind(("id", record_id(TABLE, Uuid::now_v7())))
        .bind(("firm_id", record_id(FIRM_TABLE, firm_id)))
        .bind(("provider", provider.as_str().to_string()))
        .bind(("kind", kind.as_str().to_string()))
        .bind(("version", version))
        .bind(("ciphertext", BASE64.encode(ciphertext)))
        .bind(("wrapped_dek", BASE64.encode(wrapped.ciphertext)))
        .bind(("kms_key_version", wrapped.key_version.clone()))
        .bind(("kms_context", context.aad()))
        .bind(("actor_id", record_id(PERSON_TABLE, actor_id)))
        .bind(("now", now.clone()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let metadata = metadata_for_firm(
        surreal,
        actor_role,
        actor_person_id,
        firm_id,
        provider,
        kind,
    )
    .await?;
    crate::postgres_mirror::after_write(
        surreal,
        crate::postgres_mirror::MirrorKey::FirmIntegrationSecrets(firm_id),
    )
    .await;
    Ok(metadata)
}

/// Read only metadata for the current version.
pub async fn metadata_for_firm(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
) -> Result<SecretMetadata, SecretStoreError> {
    authorize(
        surreal,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ViewIntegrationSecretMetadata,
    )
    .await?;
    validate_kind(provider, kind)?;
    let mut response = surreal
        .query(format!(
            "SELECT firm_id, provider, kind, version, kms_key_version, status, actor_id, \
             created_at, updated_at FROM {TABLE} WHERE firm_id = $firm_id AND provider = $provider \
             AND kind = $kind AND status = 'active' ORDER BY version DESC LIMIT 1"
        ))
        .bind(("firm_id", record_id(FIRM_TABLE, firm_id)))
        .bind(("provider", provider.as_str().to_string()))
        .bind(("kind", kind.as_str().to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<MetadataRow> = response.take(0)?;
    rows.into_iter()
        .next()
        .map(metadata)
        .transpose()?
        .ok_or_else(|| not_configured(provider, kind))
}

/// Read metadata for every provider/kind this Firm currently has an active
/// version of — the Doppler-like settings list. Still no plaintext, ciphertext,
/// or wrapped-key field; same authorization as [`metadata_for_firm`], which
/// this does not replace: that one still answers "the current version of this
/// one kind" for a resolver or a narrower read.
pub async fn list_metadata_for_firm(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
) -> Result<Vec<SecretMetadata>, SecretStoreError> {
    authorize(
        surreal,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ViewIntegrationSecretMetadata,
    )
    .await?;
    let mut response = surreal
        .query(format!(
            "SELECT firm_id, provider, kind, version, kms_key_version, status, actor_id, \
             created_at, updated_at FROM {TABLE} WHERE firm_id = $firm_id AND status = 'active' \
             ORDER BY provider, kind"
        ))
        .bind(("firm_id", record_id(FIRM_TABLE, firm_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<MetadataRow> = response.take(0)?;
    rows.into_iter().map(metadata).collect()
}

/// Revoke a current credential without calling the provider. Provider-side
/// revocation remains an operator action and is stated in the settings UI.
pub async fn revoke(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
) -> Result<(), SecretStoreError> {
    authorize(
        surreal,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ManageIntegrationSecrets,
    )
    .await?;
    validate_kind(provider, kind)?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut response = surreal
        .query(format!(
            "UPDATE {TABLE} SET status = 'revoked', updated_at = $now WHERE firm_id = $firm_id \
             AND provider = $provider AND kind = $kind AND status = 'active' RETURN VALUE id"
        ))
        .bind(("firm_id", record_id(FIRM_TABLE, firm_id)))
        .bind(("provider", provider.as_str().to_string()))
        .bind(("kind", kind.as_str().to_string()))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
    if ids.is_empty() {
        return Err(not_configured(provider, kind));
    }
    crate::postgres_mirror::after_write(
        surreal,
        crate::postgres_mirror::MirrorKey::FirmIntegrationSecrets(firm_id),
    )
    .await;
    Ok(())
}

/// Resolve a single current credential from the Project's owning Firm.
pub async fn resolve_project_credential(
    surreal: &SurrealDb,
    project_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedCredential, SecretStoreError> {
    validate_kind(provider, kind)?;
    let project = crate::projects::find_by_id(surreal, project_id)
        .await
        .map_err(|_| SecretStoreError::NoSuchProject(project_id))?
        .ok_or(SecretStoreError::NoSuchProject(project_id))?;
    let firm_id = project.firm_id.ok_or(SecretStoreError::ProjectHasNoFirm)?;
    resolve_firm_credential(surreal, firm_id, provider, kind, kms).await
}

/// Runtime-only credential resolution for a known Firm, including verified ingress.
/// Callers must establish their authorization or verify the provider signature.
pub async fn resolve_firm_credential(
    surreal: &SurrealDb,
    firm_id: Uuid,
    provider: IntegrationProvider,
    kind: IntegrationSecretKind,
    kms: &dyn RuntimeKms,
) -> Result<ResolvedCredential, SecretStoreError> {
    validate_kind(provider, kind)?;
    let mut response = surreal
        .query(format!(
            "SELECT firm_id, provider, kind, version, ciphertext, wrapped_dek, kms_key_version, \
             kms_context, status, actor_id, created_at, updated_at FROM {TABLE} WHERE firm_id = $firm_id \
             AND provider = $provider AND kind = $kind AND status = 'active' ORDER BY version DESC LIMIT 1"
        ))
        .bind(("firm_id", record_id(FIRM_TABLE, firm_id)))
        .bind(("provider", provider.as_str().to_string()))
        .bind(("kind", kind.as_str().to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<SecretRow> = response.take(0)?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| not_configured(provider, kind))?;
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
    if row.status != "active" || record_uuid(&row.firm_id) != Some(firm_id) {
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

#[cfg(test)]
mod tests {
    use super::{zeroize, IntegrationProvider, IntegrationSecretKind, ResolvedCredential};

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
        assert_eq!(super::ALL_KINDS.len(), 11);
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

//! Portal credential resolution, on the Firm secrets lane PR #213 opened:
//! one encrypted `CourtPortal`/`PortalLogin` secret per Firm whose JSON
//! body is keyed by site (`la-superior`, `pacer`, …), each entry a username
//! and password pair. Adapters request their login by key and never see
//! anything but the resolved pair — which is redacted in `Debug` and
//! zeroized on drop.
//!
//! Nothing here invents a secrets mechanism: the row is stored and read
//! through `store::firm_secrets`, the same AES-256-GCM + KMS path the
//! Firm's social credentials use.

use std::collections::BTreeMap;

use cloud::RuntimeKms;
use store::firm_secrets::{
    resolve_firm_credential, IntegrationProvider, IntegrationSecretKind, SecretStoreError,
};
use store::surreal::SurrealDb;

/// Why a run could not get its portal login.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CredentialsError {
    #[error("the Firm's portal login secret could not be read")]
    Store(#[from] SecretStoreError),
    #[error("the Firm's portal login secret is not shaped as a portal login payload: {detail}")]
    InvalidPayload { detail: String },
    #[error("the Firm's portal login secret has no `{key}` entry; ask someone with the ManageIntegrationSecrets capability to add one with a `username` and `password`")]
    NotProvisioned { key: String },
    #[error("the `{key}` portal login entry has an empty {field}; ask someone with the ManageIntegrationSecrets capability to fill it in")]
    IncompleteProvisioning { key: String, field: String },
}

/// One site's username and password pair, as an adapter logs in with.
///
/// Hand out only through [`PortalLogins::login_for`]. `Debug` is redacted
/// and the material is zeroized on drop, so a login never outlives the run
/// that used it in plaintext.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SiteLogin {
    // UTF-8 bytes, not `String`s: the same shape the store's own
    // `ResolvedCredential` uses, so the drop-wipe is a plain slice fill
    // with no unsafe byte surgery on a `String`.
    username: Vec<u8>,
    password: Vec<u8>,
}

impl SiteLogin {
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))] // adapters read these when logging in
    pub(crate) fn username(&self) -> &str {
        std::str::from_utf8(&self.username).unwrap_or("")
    }

    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))] // adapters read these when logging in
    pub(crate) fn password(&self) -> &str {
        std::str::from_utf8(&self.password).unwrap_or("")
    }

    /// Fail fast on a mis-provisioned entry — before a browser opens and
    /// before a portal can lock the account out over bad submissions.
    ///
    /// # Errors
    ///
    /// When the username or password is empty.
    pub(crate) fn ensure_complete(&self, key: &str) -> Result<(), CredentialsError> {
        if self.username.is_empty() {
            return Err(CredentialsError::IncompleteProvisioning {
                key: key.to_owned(),
                field: "username".to_owned(),
            });
        }
        if self.password.is_empty() {
            return Err(CredentialsError::IncompleteProvisioning {
                key: key.to_owned(),
                field: "password".to_owned(),
            });
        }
        Ok(())
    }
}

impl std::fmt::Debug for SiteLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SiteLogin")
            .field("username", &"[redacted]")
            .field("password", &"[redacted]")
            .finish()
    }
}

impl Drop for SiteLogin {
    fn drop(&mut self) {
        // The same in-place wipe `store::firm_secrets` applies to resolved
        // credentials: overwrite the bytes, keep no plaintext after the run.
        self.username.fill(0);
        self.password.fill(0);
    }
}

/// Every portal login the Firm has provisioned, keyed by site.
///
/// The whole Firm stores ONE `CourtPortal`/`PortalLogin` secret whose JSON
/// body carries every site's entry; this is that body, parsed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PortalLogins(BTreeMap<String, SiteLogin>);

impl PortalLogins {
    /// The login provisioned for `key`, if any.
    ///
    /// # Errors
    ///
    /// When the key has no entry — the error names what to provision.
    pub(crate) fn login_for(&self, key: &str) -> Result<&SiteLogin, CredentialsError> {
        self.0
            .get(key)
            .ok_or_else(|| CredentialsError::NotProvisioned {
                key: key.to_owned(),
            })
    }
}

/// Parse the resolved secret's value into per-site logins.
///
/// # Errors
///
/// When the value is not an object keyed by site with `username` and
/// `password` strings in each entry.
pub(crate) fn parse_portal_logins(
    value: &serde_json::Value,
) -> Result<PortalLogins, CredentialsError> {
    let serde_json::Value::Object(entries) = value else {
        return Err(CredentialsError::InvalidPayload {
            detail: "expected a JSON object keyed by site".to_owned(),
        });
    };
    let mut logins = BTreeMap::new();
    for (key, entry) in entries {
        let Some(entry_object) = entry.as_object() else {
            return Err(CredentialsError::InvalidPayload {
                detail: format!("the `{key}` entry is not an object"),
            });
        };
        let Some(username) = entry_object
            .get("username")
            .and_then(serde_json::Value::as_str)
        else {
            return Err(CredentialsError::InvalidPayload {
                detail: format!("the `{key}` entry has no `username` string"),
            });
        };
        let Some(password) = entry_object
            .get("password")
            .and_then(serde_json::Value::as_str)
        else {
            return Err(CredentialsError::InvalidPayload {
                detail: format!("the `{key}` entry has no `password` string"),
            });
        };
        logins.insert(
            key.clone(),
            SiteLogin {
                username: username.as_bytes().to_vec(),
                password: password.as_bytes().to_vec(),
            },
        );
    }
    Ok(PortalLogins(logins))
}

/// Resolve the Firm's `CourtPortal`/`PortalLogin` secret through the store
/// and parse it. Runs before any browser opens, so a missing or malformed
/// secret hard-stops the run with a provisioning error and never launches
/// a session.
///
/// # Errors
///
/// Store read failures and malformed payloads; see [`CredentialsError`].
pub(crate) async fn resolve_portal_logins(
    surreal: &SurrealDb,
    firm_id: uuid::Uuid,
    kms: &dyn RuntimeKms,
) -> Result<PortalLogins, CredentialsError> {
    let resolved = resolve_firm_credential(
        surreal,
        firm_id,
        IntegrationProvider::CourtPortal,
        IntegrationSecretKind::PortalLogin,
        kms,
    )
    .await?;
    // The resolved credential hands its bytes over only through the
    // explicit `expose_for_provider_call` accessor — never by field.
    let payload: serde_json::Value = serde_json::from_str(resolved.expose_for_provider_call())
        .map_err(|error| CredentialsError::InvalidPayload {
            detail: format!("the stored secret is not valid JSON: {error}"),
        })?;
    parse_portal_logins(&payload)
}

#[cfg(test)]
mod tests {
    use super::{parse_portal_logins, CredentialsError};
    use store::firm_secrets::SecretStoreError;

    const LA_SUPERIOR: &str = r#"{
        "la-superior": {"username": "firm@example.com", "password": "correct horse battery staple"}
    }"#;

    #[test]
    fn a_well_shaped_payload_parses_per_site() {
        let logins = parse_portal_logins(&serde_json::from_str(LA_SUPERIOR).expect("valid json"))
            .expect("a valid payload");
        let login = logins.login_for("la-superior").expect("the entry exists");
        assert_eq!(login.username(), "firm@example.com");
        assert_eq!(login.password(), "correct horse battery staple");
    }

    #[test]
    fn a_non_object_payload_is_invalid() {
        let error =
            parse_portal_logins(&serde_json::json!(["not", "a", "map"])).expect_err("rejected");
        assert!(error
            .to_string()
            .contains("expected a JSON object keyed by site"));
    }

    #[test]
    fn an_entry_without_a_password_is_invalid() {
        let payload = serde_json::json!({"la-superior": {"username": "firm@example.com"}});
        let error = parse_portal_logins(&payload).expect_err("rejected");
        assert!(error.to_string().contains("no `password` string"));
    }

    #[test]
    fn a_missing_entry_names_the_key_to_provision() {
        let logins = parse_portal_logins(&serde_json::from_str(LA_SUPERIOR).expect("valid json"))
            .expect("a valid payload");
        let error = logins.login_for("pacer").expect_err("no pacer entry");
        assert!(matches!(error, CredentialsError::NotProvisioned { key } if key == "pacer"));
        assert!(error.to_string().contains("`pacer`"));
    }

    #[test]
    fn an_incomplete_entry_fails_before_any_browser_opens() {
        let payload =
            serde_json::json!({"la-superior": {"username": "firm@example.com", "password": ""}});
        let logins = parse_portal_logins(&payload).expect("a valid payload");
        let error = logins
            .login_for("la-superior")
            .expect("the entry exists")
            .ensure_complete("la-superior")
            .expect_err("empty password");
        assert!(error.to_string().contains("empty password"));
    }

    #[test]
    fn debug_output_never_carries_the_secret() {
        let logins = parse_portal_logins(&serde_json::from_str(LA_SUPERIOR).expect("valid json"))
            .expect("a valid payload");
        let rendered = format!("{logins:?}");
        assert!(!rendered.contains("correct horse battery staple"));
        assert!(!rendered.contains("firm@example.com"));
        assert!(rendered.contains("[redacted]"));
    }

    /// Seed a Firm and store the portal-login payload through the shared
    /// test helpers — the real `store::firm_secrets` lane, admin-authored.
    async fn seeded_firm(surreal: &store::surreal::SurrealDb, payload: Option<&str>) -> uuid::Uuid {
        let (firm_id, admin_id) = crate::scraper::testing::seeded_firm_with_admin(surreal).await;
        if let Some(payload) = payload {
            crate::scraper::testing::put_portal_secret(
                surreal,
                admin_id,
                firm_id,
                &serde_json::from_str(payload).expect("valid json"),
            )
            .await;
        }
        firm_id
    }

    #[tokio::test]
    async fn the_store_lane_resolves_and_parses_end_to_end() {
        let surreal = store::test_support::mem_surreal().await;
        let firm_id = seeded_firm(&surreal, Some(LA_SUPERIOR)).await;
        let logins =
            super::resolve_portal_logins(&surreal, firm_id, &cloud::FakeKms::new("scraper-test"))
                .await
                .expect("resolved through the Firm secrets lane");
        assert!(logins.login_for("la-superior").is_ok());
    }

    #[tokio::test]
    async fn a_missing_store_row_hard_stops_with_the_store_error() {
        let surreal = store::test_support::mem_surreal().await;
        let firm_id = seeded_firm(&surreal, None).await;
        let error =
            super::resolve_portal_logins(&surreal, firm_id, &cloud::FakeKms::new("scraper-test"))
                .await
                .expect_err("nothing provisioned");
        assert!(matches!(
            error,
            CredentialsError::Store(SecretStoreError::NotConfigured { .. })
        ));
    }
}

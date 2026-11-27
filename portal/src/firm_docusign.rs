//! Resolve DocuSign credentials inside the owning Firm's boundary.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use store::{
    application_store::ApplicationStore,
    firm_secrets::{resolve_firm_credential, IntegrationProvider, IntegrationSecretKind},
};
use uuid::Uuid;

use crate::{
    docusign_auth::DocuSignJwtAuth,
    signature::{
        CompletedDocuments, DocuSignSignatureProvider, RecipientView, SendMode, SignatureError,
        SignatureManifest, SignatureProvider, SignatureRequestId,
    },
};

#[derive(Deserialize)]
struct Connection {
    account_id: Uuid,
    integration_key: Uuid,
    user_id: Uuid,
    private_key_pem: String,
    base_url: String,
    signer_email: String,
    signer_name: String,
    connect_hmac_key: Option<String>,
}

impl Connection {
    fn provider(self, demo: bool) -> Result<DocuSignSignatureProvider, SignatureError> {
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|_| unavailable("its base_url is not a URL"))?;
        let permitted_host = url.host_str().is_some_and(|host| {
            if demo {
                host == "demo.docusign.net"
            } else {
                host != "demo.docusign.net" && host.ends_with(".docusign.net")
            }
        });
        if !permitted_host
            || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path().trim_end_matches('/') != "/restapi"
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port().is_some()
        {
            return Err(unavailable(if demo {
                "its base_url must be https://demo.docusign.net/restapi on a simulated deployment"
            } else {
                "its base_url must be a live https://<host>.docusign.net/restapi"
            }));
        }
        let oauth = if demo {
            "https://account-d.docusign.com"
        } else {
            "https://account.docusign.com"
        };
        Ok(DocuSignSignatureProvider::with_jwt_auth(
            url.to_string(),
            self.account_id.to_string(),
            DocuSignJwtAuth::new(
                self.integration_key.to_string(),
                self.user_id.to_string(),
                oauth,
                self.private_key_pem.into_bytes(),
            ),
            self.signer_email,
            self.signer_name,
        ))
    }
}

/// The refusal every unresolvable connection answers with. `reason` names
/// which check failed and never carries a credential byte, so it is safe in
/// logs and in the API response a Firm administrator acts on.
fn unavailable(reason: &str) -> SignatureError {
    SignatureError::Connection(reason.to_owned())
}

/// serde's message can quote the offending value, so only its category and
/// position leave this function.
fn parse_connection(json: &str) -> Result<Connection, SignatureError> {
    serde_json::from_str(json).map_err(|error| {
        unavailable(&format!(
            "the saved connection JSON is invalid ({:?} at line {}, column {})",
            error.classify(),
            error.line(),
            error.column()
        ))
    })
}

const NOT_CONNECTED: &str = "this provider resolves a Firm connection per Project";

pub(crate) struct FirmDocuSign {
    pub kms: Arc<dyn cloud::RuntimeKms>,
    pub demo: bool,
}

impl FirmDocuSign {
    pub(crate) async fn for_firm(
        &self,
        db: &ApplicationStore,
        firm: Uuid,
    ) -> Result<DocuSignSignatureProvider, SignatureError> {
        self.connection(db, firm).await?.provider(self.demo)
    }

    pub(crate) async fn connect_key(
        &self,
        db: &ApplicationStore,
        firm: Uuid,
    ) -> Result<(Uuid, String), SignatureError> {
        let connection = self.connection(db, firm).await?;
        let account = connection.account_id;
        let key = connection
            .connect_hmac_key
            .filter(|value| !value.is_empty())
            .ok_or_else(|| unavailable("it has no connect_hmac_key"))?;
        Ok((account, key))
    }

    async fn connection(
        &self,
        db: &ApplicationStore,
        firm: Uuid,
    ) -> Result<Connection, SignatureError> {
        let credential = resolve_firm_credential(
            db,
            firm,
            IntegrationProvider::DocuSign,
            IntegrationSecretKind::DocuSignConnection,
            self.kms.as_ref(),
        )
        .await
        .map_err(|error| unavailable(&error.to_string()))?;
        parse_connection(credential.expose_for_provider_call())
    }
}

#[async_trait]
impl SignatureProvider for FirmDocuSign {
    async fn for_project(
        &self,
        db: &ApplicationStore,
        project_id: Uuid,
    ) -> Result<Option<DocuSignSignatureProvider>, SignatureError> {
        let pg = store::postgres::PostgresDb::from_env()
            .await
            .map_err(|_| unavailable("PostgreSQL is not configured"))?;
        let firm = store::postgres::projects::find_by_id_on(&pg, project_id)
            .await
            .map_err(|_| unavailable("the Project lookup failed"))?
            .ok_or_else(|| unavailable("the Project is not in PostgreSQL"))?
            .firm_id
            .ok_or_else(|| unavailable("the Project has no owning Firm"))?;
        self.for_firm(db, firm).await.map(Some)
    }

    async fn for_firm(
        &self,
        db: &ApplicationStore,
        firm: Uuid,
    ) -> Result<Option<DocuSignSignatureProvider>, SignatureError> {
        FirmDocuSign::for_firm(self, db, firm).await.map(Some)
    }

    async fn verify_firm_connect(
        &self,
        db: &ApplicationStore,
        firm: Uuid,
        signature: &str,
        body: &[u8],
    ) -> Option<Uuid> {
        let (account, key) = self.connect_key(db, firm).await.ok()?;
        crate::webhook_auth::verify_hmac_sha256_b64(key.as_bytes(), body, signature)
            .then_some(account)
    }

    fn supports_firm_connect(&self) -> bool {
        true
    }

    async fn send_for_signature(
        &self,
        _: Uuid,
        _: &[u8],
        _: &SignatureManifest,
        _: SendMode,
    ) -> Result<SignatureRequestId, SignatureError> {
        Err(unavailable(NOT_CONNECTED))
    }

    async fn dispatch_prepared(&self, _: &SignatureRequestId) -> Result<(), SignatureError> {
        Err(unavailable(NOT_CONNECTED))
    }

    async fn create_recipient_view(
        &self,
        _: &SignatureRequestId,
        _: &RecipientView,
    ) -> Result<String, SignatureError> {
        Err(unavailable(NOT_CONNECTED))
    }

    async fn create_sender_view(
        &self,
        _: &SignatureRequestId,
        _: &str,
    ) -> Result<String, SignatureError> {
        Err(unavailable(NOT_CONNECTED))
    }

    async fn fetch_completed_documents(
        &self,
        _: &SignatureRequestId,
    ) -> Result<CompletedDocuments, SignatureError> {
        Err(unavailable(NOT_CONNECTED))
    }
}

pub(crate) async fn for_notation(
    provider: &dyn SignatureProvider,
    db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<DocuSignSignatureProvider>, SignatureError> {
    let notation = store::notations::find_by_id(db, notation_id)
        .await
        .map_err(|_| unavailable("the Notation lookup failed"))?
        .ok_or_else(|| unavailable("the Notation does not exist"))?;
    provider.for_project(db, notation.project_id).await
}

pub(crate) fn resolved<'a>(
    scoped: Option<&'a DocuSignSignatureProvider>,
    local: &'a dyn SignatureProvider,
) -> &'a dyn SignatureProvider {
    scoped.map_or(local, |provider| provider)
}

#[cfg(test)]
mod tests {
    use super::{parse_connection, Connection};
    use uuid::Uuid;

    fn connection(base_url: &str) -> Connection {
        Connection {
            account_id: Uuid::new_v4(),
            integration_key: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            private_key_pem: "synthetic".into(),
            base_url: base_url.into(),
            signer_email: "signer@example.com".into(),
            signer_name: "Example Signer".into(),
            connect_hmac_key: None,
        }
    }

    #[test]
    fn refuses_urls_that_could_send_credentials_to_the_wrong_host() {
        for url in [
            "http://na4.docusign.net/restapi",
            "https://docusign.net.example.com/restapi",
            "https://docusign.net@elsewhere.example.com/restapi",
            "https://na4.docusign.net/restapi?query=bad",
            "https://demo.docusign.net/restapi",
        ] {
            assert!(connection(url).provider(false).is_err(), "{url}");
        }
        assert!(connection("https://na4.docusign.net/restapi")
            .provider(false)
            .is_ok());
        assert!(connection("https://na4.docusign.net/restapi")
            .provider(true)
            .is_err());
        assert!(connection("https://demo.docusign.net/restapi")
            .provider(true)
            .is_ok());
    }

    #[test]
    fn a_refusal_names_the_check_that_failed() {
        let Err(error) = connection("https://demo.docusign.net/restapi").provider(false) else {
            panic!("a demo host must not serve a live deployment");
        };
        assert!(
            error
                .to_string()
                .contains("live https://<host>.docusign.net/restapi"),
            "{error}"
        );
    }

    #[test]
    fn invalid_connection_json_never_echoes_the_secret() {
        let Err(error) = parse_connection(r#"{"account_id":"synthetic-secret-value"}"#) else {
            panic!("a non-UUID account_id must not parse");
        };
        let message = error.to_string();
        assert!(message.contains("connection JSON is invalid"), "{message}");
        assert!(!message.contains("synthetic-secret-value"), "{message}");
    }
}

//! Resolve DocuSign credentials inside the owning Firm's boundary.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use store::{
    firm_secrets::{resolve_firm_credential, IntegrationProvider, IntegrationSecretKind},
    surreal::SurrealDb,
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
}

impl Connection {
    fn provider(self, demo: bool) -> Result<DocuSignSignatureProvider, SignatureError> {
        let url = reqwest::Url::parse(&self.base_url).map_err(|_| unavailable())?;
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
            return Err(unavailable());
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

fn unavailable() -> SignatureError {
    SignatureError::Provider("The Firm's DocuSign connection is unavailable".into())
}

pub(crate) struct FirmDocuSign {
    pub kms: Arc<dyn cloud::RuntimeKms>,
    pub demo: bool,
}

impl FirmDocuSign {
    pub(crate) async fn for_firm(
        &self,
        db: &SurrealDb,
        firm: Uuid,
    ) -> Result<DocuSignSignatureProvider, SignatureError> {
        let credential = resolve_firm_credential(
            db,
            firm,
            IntegrationProvider::DocuSign,
            IntegrationSecretKind::DocuSignConnection,
            self.kms.as_ref(),
        )
        .await
        .map_err(|_| unavailable())?;
        let connection: Connection = serde_json::from_str(credential.expose_for_provider_call())
            .map_err(|_| unavailable())?;
        connection.provider(self.demo)
    }
}

#[async_trait]
impl SignatureProvider for FirmDocuSign {
    async fn for_project(
        &self,
        db: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Option<DocuSignSignatureProvider>, SignatureError> {
        let firm = store::projects::find_by_id(db, project_id)
            .await
            .map_err(|_| unavailable())?
            .and_then(|project| project.firm_id)
            .ok_or_else(unavailable)?;
        self.for_firm(db, firm).await.map(Some)
    }

    async fn send_for_signature(
        &self,
        _: Uuid,
        _: &[u8],
        _: &SignatureManifest,
        _: SendMode,
    ) -> Result<SignatureRequestId, SignatureError> {
        Err(unavailable())
    }

    async fn dispatch_prepared(&self, _: &SignatureRequestId) -> Result<(), SignatureError> {
        Err(unavailable())
    }

    async fn create_recipient_view(
        &self,
        _: &SignatureRequestId,
        _: &RecipientView,
    ) -> Result<String, SignatureError> {
        Err(unavailable())
    }

    async fn create_sender_view(
        &self,
        _: &SignatureRequestId,
        _: &str,
    ) -> Result<String, SignatureError> {
        Err(unavailable())
    }

    async fn fetch_completed_documents(
        &self,
        _: &SignatureRequestId,
    ) -> Result<CompletedDocuments, SignatureError> {
        Err(unavailable())
    }
}

pub(crate) async fn for_notation(
    provider: &dyn SignatureProvider,
    db: &SurrealDb,
    notation_id: Uuid,
) -> Result<Option<DocuSignSignatureProvider>, SignatureError> {
    let notation = store::notations::find_by_id(db, notation_id)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    provider.for_project(db, notation.project_id).await
}

pub(crate) fn resolved<'a>(
    scoped: &'a Option<DocuSignSignatureProvider>,
    local: &'a dyn SignatureProvider,
) -> &'a dyn SignatureProvider {
    scoped.as_ref().map_or(local, |provider| provider)
}

#[cfg(test)]
mod tests {
    use super::Connection;
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
}

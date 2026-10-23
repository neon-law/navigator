//! Read-only Mercury adapter. Every request names one account and transaction;
//! there is no transaction enumeration or money movement operation.
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum MercuryError {
    #[error("Mercury request failed")]
    Transport,
    #[error("Mercury returned HTTP {0}")]
    Http(reqwest::StatusCode),
    #[error("Mercury returned invalid or oversized evidence")]
    Invalid,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub id: Uuid,
    pub account_id: Uuid,
    /// Keep the original JSON numeral: never convert bank dollars to f64.
    pub amount: Box<serde_json::value::RawValue>,
    pub status: String,
    pub posted_at: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub file_name: String,
    pub url: String,
    pub attachment_type: String,
}
/// Credential formatting is deliberately absent. HTTP errors contain neither
/// the bearer token nor provider response bodies.
pub struct Mercury {
    token: String,
    client: reqwest::Client,
    base: String,
}
impl Mercury {
    pub fn new(token: &str) -> Result<Self, MercuryError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| MercuryError::Transport)?;
        Ok(Self {
            token: token.into(),
            client,
            base: "https://api.mercury.com".into(),
        })
    }
    pub async fn transaction(
        &self,
        account: Uuid,
        id: Uuid,
    ) -> Result<(Transaction, Vec<u8>), MercuryError> {
        let response = self
            .client
            .get(format!(
                "{}/api/v1/account/{account}/transaction/{id}",
                self.base
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| MercuryError::Transport)?;
        if !response.status().is_success() {
            return Err(MercuryError::Http(response.status()));
        }
        let bytes = bounded(response, 1_048_576).await?;
        let transaction: Transaction =
            serde_json::from_slice(&bytes).map_err(|_| MercuryError::Invalid)?;
        if transaction.id != id
            || transaction.account_id != account
            || transaction.attachments.len() > 100
        {
            return Err(MercuryError::Invalid);
        }
        Ok((transaction, bytes))
    }
    pub async fn receipt(&self, attachment: &Attachment) -> Result<Vec<u8>, MercuryError> {
        let url = reqwest::Url::parse(&attachment.url).map_err(|_| MercuryError::Invalid)?;
        let host = url.host_str().ok_or(MercuryError::Invalid)?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || !(host == "api.mercury.com"
                || host.ends_with(".amazonaws.com")
                || host == "storage.googleapis.com")
        {
            return Err(MercuryError::Invalid);
        }
        // Signed receipt URLs are separate requests without the Mercury token.
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| MercuryError::Transport)?;
        if !response.status().is_success() {
            return Err(MercuryError::Http(response.status()));
        }
        bounded(response, 16 * 1024 * 1024).await
    }
}
pub(crate) async fn bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, MercuryError> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(MercuryError::Invalid);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| MercuryError::Transport)?
    {
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|n| n > limit)
        {
            return Err(MercuryError::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    #[tokio::test]
    async fn reads_only_explicit_transaction_and_preserves_original_decimal() {
        let server = MockServer::start().await;
        let account = Uuid::now_v7();
        let id = Uuid::now_v7();
        Mock::given(method("GET")).and(path(format!("/api/v1/account/{account}/transaction/{id}"))).and(header("Authorization","Bearer synthetic-read-token")).respond_with(ResponseTemplate::new(200).set_body_string(format!(r#"{{"id":"{id}","accountId":"{account}","amount":1000000000000.01,"status":"sent","attachments":[]}}"#))).expect(1).mount(&server).await;
        let mut client = Mercury::new("synthetic-read-token").unwrap();
        client.base = server.uri();
        let (t, _) = client.transaction(account, id).await.unwrap();
        assert_eq!(t.amount.get(), "1000000000000.01");
        assert!(client.transaction(account, Uuid::now_v7()).await.is_err());
    }
    #[tokio::test]
    async fn receipts_refuse_untrusted_urls_before_sending_credentials() {
        let client = Mercury::new("synthetic").unwrap();
        let a = Attachment {
            file_name: "receipt".into(),
            attachment_type: "receipt".into(),
            url: "http://127.0.0.1/private".into(),
        };
        assert!(matches!(
            client.receipt(&a).await,
            Err(MercuryError::Invalid)
        ));
    }
}

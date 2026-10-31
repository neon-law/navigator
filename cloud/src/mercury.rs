//! Read-only Mercury adapter. Every request names one account; a transaction
//! read names one transaction, and a window listing enumerates one bounded
//! posted-date range over one account. There is no money movement operation.
use serde::Deserialize;
use uuid::Uuid;

/// Fixed page size for a window listing; the provider may return fewer.
const PAGE_SIZE: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum MercuryError {
    #[error("Mercury request failed")]
    Transport,
    #[error("Mercury returned HTTP {0}")]
    Http(reqwest::StatusCode),
    #[error("Mercury returned invalid or oversized evidence")]
    Invalid,
    #[error("Mercury window exceeds the transaction cap")]
    Overflow,
}
#[derive(Deserialize, serde::Serialize)]
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
    /// Free-text fields a bulk import may allocate a Project code against.
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub counterparty_name: Option<String>,
    #[serde(default)]
    pub source_description: Option<String>,
}
#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub file_name: String,
    pub url: String,
    pub attachment_type: String,
}
/// One page of a window listing response.
#[derive(Deserialize)]
pub struct TransactionPage {
    #[serde(default)]
    pub transactions: Vec<Transaction>,
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
    /// List a closed posted-date window for one account with a fixed page
    /// size, stopping at a short page, an empty page, or the caller's cap.
    /// Every listed transaction must name the requested account.
    pub async fn transactions_window(
        &self,
        account: Uuid,
        start: chrono::NaiveDate,
        end: chrono::NaiveDate,
        max_transactions: usize,
    ) -> Result<Vec<Transaction>, MercuryError> {
        let mut transactions = Vec::new();
        let mut offset = 0usize;
        // The cap bounds the loop; a page cannot return more than PAGE_SIZE.
        while offset <= max_transactions {
            let response = self
                .client
                .get(format!(
                    "{}/api/v1/account/{account}/transactions",
                    self.base
                ))
                .query(&[
                    ("limit", PAGE_SIZE.to_string().as_str()),
                    ("offset", offset.to_string().as_str()),
                    ("start", &start.to_string()),
                    ("end", &end.to_string()),
                ])
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|_| MercuryError::Transport)?;
            if !response.status().is_success() {
                return Err(MercuryError::Http(response.status()));
            }
            let bytes = bounded(response, 1_048_576).await?;
            let page: TransactionPage =
                serde_json::from_slice(&bytes).map_err(|_| MercuryError::Invalid)?;
            if page.transactions.len() > PAGE_SIZE {
                return Err(MercuryError::Invalid);
            }
            for transaction in &page.transactions {
                if transaction.account_id != account {
                    return Err(MercuryError::Invalid);
                }
            }
            offset += page.transactions.len();
            let page_len = page.transactions.len();
            transactions.extend(page.transactions);
            if transactions.len() > max_transactions {
                return Err(MercuryError::Overflow);
            }
            if page_len < PAGE_SIZE {
                return Ok(transactions);
            }
        }
        Err(MercuryError::Overflow)
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
    async fn transaction_redirects_cannot_change_the_fixed_provider_destination() {
        let source = MockServer::start().await;
        let target = MockServer::start().await;
        let account = Uuid::now_v7();
        let id = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/account/{account}/transaction/{id}")))
            .and(header("Authorization", "Bearer synthetic-read-token"))
            .respond_with(ResponseTemplate::new(307).insert_header("Location", target.uri()))
            .expect(1)
            .mount(&source)
            .await;
        let mut client = Mercury::new("synthetic-read-token").unwrap();
        assert_eq!(client.base, "https://api.mercury.com");
        client.base = source.uri();
        assert!(
            matches!(client.transaction(account, id).await, Err(MercuryError::Http(status)) if status.as_u16() == 307)
        );
        assert!(target.received_requests().await.unwrap().is_empty());
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

    fn listed_transaction(account: Uuid, id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "id": id, "accountId": account, "amount": "12.34", "status": "sent",
            "postedAt": "2026-10-01T00:00:00+00:00", "attachments": []
        })
    }

    fn listed_page(account: Uuid, count: usize) -> String {
        let transactions: Vec<serde_json::Value> = (0..count)
            .map(|_| listed_transaction(account, Uuid::now_v7()))
            .collect();
        serde_json::json!({ "transactions": transactions }).to_string()
    }

    #[tokio::test]
    async fn window_lists_with_a_fixed_page_cap_and_stops_at_a_short_page() {
        let server = MockServer::start().await;
        let account = Uuid::now_v7();
        let start = "2026-10-01";
        let end = "2026-10-31";
        let first = listed_page(account, PAGE_SIZE);
        let second = listed_page(account, 3);
        for (offset, body) in [(0usize, first), (PAGE_SIZE, second)] {
            Mock::given(method("GET"))
                .and(path(format!("/api/v1/account/{account}/transactions")))
                .and(wiremock::matchers::query_param("limit", "100"))
                .and(wiremock::matchers::query_param(
                    "offset",
                    offset.to_string(),
                ))
                .and(wiremock::matchers::query_param("start", start))
                .and(wiremock::matchers::query_param("end", end))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .expect(1)
                .mount(&server)
                .await;
        }
        let mut client = Mercury::new("synthetic-read-token").unwrap();
        client.base = server.uri();
        let window = client
            .transactions_window(
                account,
                chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
                5_000,
            )
            .await
            .unwrap();
        assert_eq!(window.len(), PAGE_SIZE + 3);
    }

    #[tokio::test]
    async fn window_refuses_a_transaction_outside_the_requested_account() {
        let server = MockServer::start().await;
        let account = Uuid::now_v7();
        let body = serde_json::json!({
            "transactions": [
                listed_transaction(account, Uuid::now_v7()),
                listed_transaction(Uuid::now_v7(), Uuid::now_v7()),
            ]
        })
        .to_string();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/account/{account}/transactions")))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        let mut client = Mercury::new("synthetic-read-token").unwrap();
        client.base = server.uri();
        assert!(matches!(
            client
                .transactions_window(
                    account,
                    chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                    chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
                    5_000,
                )
                .await,
            Err(MercuryError::Invalid)
        ));
    }

    #[tokio::test]
    async fn window_enforces_the_caller_cap() {
        let server = MockServer::start().await;
        let account = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/account/{account}/transactions")))
            .respond_with(ResponseTemplate::new(200).set_body_string(listed_page(account, 2)))
            .expect(1)
            .mount(&server)
            .await;
        let mut client = Mercury::new("synthetic-read-token").unwrap();
        client.base = server.uri();
        assert!(matches!(
            client
                .transactions_window(
                    account,
                    chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                    chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
                    1,
                )
                .await,
            Err(MercuryError::Overflow)
        ));
    }

    #[tokio::test]
    async fn window_refuses_an_oversized_or_malformed_page() {
        let server = MockServer::start().await;
        let account = Uuid::now_v7();
        for (index, body) in [
            // One transaction past the fixed page cap.
            serde_json::json!({ "transactions": (0..=PAGE_SIZE).map(|index| listed_transaction(account, Uuid::now_v7())).collect::<Vec<_>>() }).to_string(),
            "{\"transactions\":".to_string(),
        ]
        .into_iter()
        .enumerate()
        {
            server.reset().await;
            Mock::given(method("GET"))
                .and(path(format!("/api/v1/account/{account}/transactions")))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .expect(1)
                .mount(&server)
                .await;
            let mut client = Mercury::new("synthetic-read-token").unwrap();
            client.base = server.uri();
            assert!(
                matches!(
                    client
                        .transactions_window(
                            account,
                            chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                            chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
                            5_000,
                        )
                        .await,
                    Err(MercuryError::Invalid)
                ),
                "case {index}"
            );
        }
    }
}

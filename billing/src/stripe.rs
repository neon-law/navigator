//! Stripe draft invoicing on the account authenticated by a Firm's own key.

use reqwest::{Client, Method, Url};
use serde::Deserialize;
use uuid::Uuid;

use crate::{BillingError, InvoiceId, InvoiceRequest};

pub struct StripeInvoiceClient<'a> {
    client: Client,
    base: Url,
    key: &'a str,
}

#[derive(Deserialize)]
struct Account {
    id: String,
}

#[derive(Deserialize)]
struct StripeObject {
    id: String,
    livemode: bool,
}

#[derive(Deserialize)]
struct Invoice {
    id: String,
    livemode: bool,
    auto_advance: bool,
    status: String,
    currency: String,
    total: i64,
}

fn provider_error(message: &str) -> BillingError {
    BillingError::Provider(message.to_owned())
}

impl<'a> StripeInvoiceClient<'a> {
    pub fn new(key: &'a str, live: bool) -> Result<Self, BillingError> {
        let key = key.trim();
        let valid_prefix = if live {
            key.starts_with("sk_live_") || key.starts_with("rk_live_")
        } else {
            key.starts_with("sk_test_") || key.starts_with("rk_test_")
        };
        if !valid_prefix {
            return Err(provider_error(
                "Stripe billing credential is missing or has the wrong mode",
            ));
        }
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|_| provider_error("Stripe client unavailable"))?,
            base: Url::parse("https://api.stripe.com/v1/")
                .map_err(|_| provider_error("Stripe API configuration invalid"))?,
            key,
        })
    }

    pub async fn account_id(&self) -> Result<String, BillingError> {
        let account: Account = self.call(Method::GET, "account", None, &[]).await?;
        if !account.id.starts_with("acct_") {
            return Err(provider_error(
                "Stripe returned an invalid account identity",
            ));
        }
        Ok(account.id)
    }

    /// Create and confirm an unsent draft. All writes use the batch's stable
    /// key and explicitly attach items to the invoice they belong to.
    pub async fn create_draft(
        &self,
        batch: Uuid,
        request: &InvoiceRequest,
        iolta: bool,
    ) -> Result<InvoiceId, BillingError> {
        let due = request
            .due_date
            .ok_or_else(|| provider_error("invoice due date missing"))?;
        let due = due
            .and_hms_opt(23, 59, 59)
            .ok_or_else(|| provider_error("invoice due date invalid"))?
            .and_utc()
            .timestamp()
            .to_string();
        let live = self.key.starts_with("sk_live_") || self.key.starts_with("rk_live_");
        let customer: StripeObject = self
            .call(
                Method::POST,
                "customers",
                Some(&format!("{batch}/customer")),
                &[
                    ("name", &request.contact_name),
                    ("email", &request.contact_email),
                ],
            )
            .await?;
        if customer.livemode != live || !customer.id.starts_with("cus_") {
            return Err(provider_error("Stripe returned an invalid customer"));
        }
        let description = if iolta {
            request.reference.clone()
        } else {
            format!(
                "{}. Operating account. This is not IOLTA billing.",
                request.reference
            )
        };
        let invoice: StripeObject = self
            .call(
                Method::POST,
                "invoices",
                Some(&format!("{batch}/invoice")),
                &[
                    ("customer", &customer.id),
                    ("auto_advance", "false"),
                    ("collection_method", "send_invoice"),
                    ("due_date", &due),
                    ("description", &description),
                    ("pending_invoice_items_behavior", "exclude"),
                    ("metadata[navigator_batch_id]", &batch.to_string()),
                ],
            )
            .await?;
        if invoice.livemode != live || !invoice.id.starts_with("in_") {
            return Err(provider_error("Stripe returned an invalid invoice"));
        }
        let mut total = 0i64;
        for (index, line) in request.line_items.iter().enumerate() {
            let amount = line
                .unit_amount_cents
                .checked_mul(i64::from(line.quantity))
                .ok_or_else(|| provider_error("invoice line amount overflow"))?;
            total = total
                .checked_add(amount)
                .ok_or_else(|| provider_error("invoice total overflow"))?;
            let _: StripeObject = self
                .call(
                    Method::POST,
                    "invoiceitems",
                    Some(&format!("{batch}/item/{index}")),
                    &[
                        ("customer", &customer.id),
                        ("invoice", &invoice.id),
                        ("currency", "usd"),
                        ("amount", &amount.to_string()),
                        ("description", &line.description),
                    ],
                )
                .await?;
        }
        let draft: Invoice = self
            .call(Method::GET, &format!("invoices/{}", invoice.id), None, &[])
            .await?;
        if draft.livemode != live
            || draft.id != invoice.id
            || draft.status != "draft"
            || draft.auto_advance
            || draft.currency != "usd"
            || draft.total != total
        {
            return Err(provider_error(
                "Stripe draft does not match the reserved invoice",
            ));
        }
        Ok(InvoiceId(draft.id))
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        fields: &[(&str, &str)],
    ) -> Result<T, BillingError> {
        let url = self
            .base
            .join(path)
            .map_err(|_| provider_error("Stripe API path invalid"))?;
        let mut request = self.client.request(method, url).bearer_auth(self.key);
        if let Some(key) = key {
            request = request.header("Idempotency-Key", key).form(fields);
        }
        request
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| provider_error("Stripe request failed"))?
            .json()
            .await
            .map_err(|_| provider_error("Stripe response invalid"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InvoiceLine;
    use serde_json::json;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    #[test]
    fn a_key_for_the_other_mode_cannot_create_invoices() {
        assert!(StripeInvoiceClient::new("", false).is_err());
        assert!(StripeInvoiceClient::new("sk_live_synthetic", false).is_err());
        assert!(StripeInvoiceClient::new("rk_test_synthetic", true).is_err());
    }

    #[tokio::test]
    async fn an_operating_draft_is_labeled_and_items_attach_to_the_exact_invoice() {
        let server = MockServer::start().await;
        let batch = Uuid::nil();
        let mut client = StripeInvoiceClient::new("sk_test_synthetic", false).unwrap();
        client.base = Url::parse(&format!("{}/v1/", server.uri())).unwrap();

        for (path, step, object_id) in [
            ("customers", "customer", "cus_synthetic"),
            ("invoices", "invoice", "in_synthetic"),
            ("invoiceitems", "item/0", "ii_synthetic"),
        ] {
            let mut mock = Mock::given(matchers::method("POST"))
                .and(matchers::path(format!("/v1/{path}")))
                .and(matchers::header(
                    "Authorization",
                    "Bearer sk_test_synthetic",
                ))
                .and(matchers::header(
                    "Idempotency-Key",
                    format!("{batch}/{step}"),
                ));
            if step == "invoice" {
                mock = mock
                    .and(matchers::body_string_contains("auto_advance=false"))
                    .and(matchers::body_string_contains("This+is+not+IOLTA+billing"));
            }
            if step == "item/0" {
                mock = mock
                    .and(matchers::body_string_contains("invoice=in_synthetic"))
                    .and(matchers::body_string_contains("amount=1234"));
            }
            mock.respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": object_id, "livemode": false
            })))
            .expect(1)
            .mount(&server)
            .await;
        }
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/v1/invoices/in_synthetic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "in_synthetic", "livemode": false, "auto_advance": false,
                "status": "draft", "currency": "usd", "total": 1234
            })))
            .expect(1)
            .mount(&server)
            .await;
        let draft = client
            .create_draft(
                batch,
                &InvoiceRequest {
                    contact_id: None,
                    contact_name: "Synthetic Client".into(),
                    contact_email: "synthetic@example.test".into(),
                    reference: "Matter synthetic".into(),
                    line_items: vec![InvoiceLine {
                        description: "Synthetic work".into(),
                        quantity: 2,
                        unit_amount_cents: 617,
                        account_code: "200".into(),
                    }],
                    issued_date: None,
                    due_date: chrono::NaiveDate::from_ymd_opt(2026, 11, 30),
                },
                false,
            )
            .await
            .unwrap();
        assert_eq!(draft.0, "in_synthetic");
    }
}

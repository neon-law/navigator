//! Stripe draft invoices and hosted subscriptions on the account authenticated by a Firm's own key.

use reqwest::{Client, Method, Url};
use serde::Deserialize;
use uuid::Uuid;

use crate::{BillingError, InvoiceId, InvoiceRequest};

pub struct StripeInvoiceClient<'a> {
    client: Client,
    base: Url,
    key: &'a str,
    connected_trust: Option<String>,
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
struct Customer {
    id: String,
    livemode: bool,
    #[serde(default)]
    deleted: bool,
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
            connected_trust: None,
        })
    }

    /// Scope every request, including customer creation, to a reviewed trust account.
    pub async fn on_connected_trust(mut self, id: &str) -> Result<Self, BillingError> {
        if !stripe_identifier(id, "acct_") || self.account_id().await? == id {
            return Err(provider_error(
                "a separate connected trust account is required",
            ));
        }
        self.connected_trust = Some(id.to_string());
        if self.account_id().await? != id {
            return Err(provider_error("Stripe trust account identity mismatch"));
        }
        Ok(self)
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

    /// The billed party's Stripe Customer on this account: the saved one when
    /// it still exists, else a new one created under `key`'s idempotency.
    pub async fn ensure_customer(
        &self,
        saved: Option<&str>,
        key: Uuid,
        name: &str,
        email: &str,
    ) -> Result<String, BillingError> {
        let live = self.live_mode();
        if let Some(saved) = saved.filter(|id| stripe_identifier(id, "cus_")) {
            let customer: Customer = self
                .call(Method::GET, &format!("customers/{saved}"), None, &[])
                .await?;
            if customer.id == saved && customer.livemode == live && !customer.deleted {
                return Ok(customer.id);
            }
        }
        let customer: StripeObject = self
            .call(
                Method::POST,
                "customers",
                Some(&format!("{key}/customer")),
                &[("name", name), ("email", email)],
            )
            .await?;
        if customer.livemode != live || !customer.id.starts_with("cus_") {
            return Err(provider_error("Stripe returned an invalid customer"));
        }
        Ok(customer.id)
    }

    /// Create and confirm an unsent draft for a known customer. All writes
    /// use the batch's stable key and attach items to their invoice.
    pub async fn create_draft(
        &self,
        batch: Uuid,
        request: &InvoiceRequest,
        customer: &str,
    ) -> Result<InvoiceId, BillingError> {
        let due = request
            .due_date
            .ok_or_else(|| provider_error("invoice due date missing"))?
            .and_hms_opt(23, 59, 59)
            .ok_or_else(|| provider_error("invoice due date invalid"))?
            .and_utc()
            .timestamp()
            .to_string();
        self.create_draft_for_customer(batch, request, customer, &due)
            .await
    }

    /// Create and confirm an unsent draft. All writes use the batch's stable
    /// key and explicitly attach items to the invoice they belong to.
    pub async fn create_trust_draft(
        &self,
        batch: Uuid,
        request: &InvoiceRequest,
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
        let live = self.live_mode();
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
        self.create_draft_for_customer(batch, request, &customer.id, &due)
            .await
    }

    async fn create_draft_for_customer(
        &self,
        batch: Uuid,
        request: &InvoiceRequest,
        customer: &str,
        due: &str,
    ) -> Result<InvoiceId, BillingError> {
        let invoice: StripeObject = self
            .call(
                Method::POST,
                "invoices",
                Some(&format!("{batch}/invoice")),
                &[
                    ("customer", customer),
                    ("auto_advance", "false"),
                    ("collection_method", "send_invoice"),
                    ("due_date", due),
                    ("description", &request.reference),
                    ("pending_invoice_items_behavior", "exclude"),
                    ("metadata[navigator_batch_id]", &batch.to_string()),
                ],
            )
            .await?;
        if invoice.livemode != self.live_mode() || !invoice.id.starts_with("in_") {
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
                        ("customer", customer),
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
        if draft.livemode != self.live_mode()
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
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(self.key)
            .header("Stripe-Version", "2026-10-28.endive");
        if let Some(account) = &self.connected_trust {
            request = request.header("Stripe-Account", account);
        }
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
    async fn a_trust_draft_keeps_all_items_in_the_reviewed_connected_account() {
        let server = MockServer::start().await;
        let account_read = Mock::given(matchers::method("GET")).and(matchers::path("/v1/account"));
        account_read
            .respond_with(|request: &wiremock::Request| {
                let account = if request.headers.get("Stripe-Account").is_some() {
                    "acct_Trust"
                } else {
                    "acct_Platform"
                };
                ResponseTemplate::new(200).set_body_json(json!({"id":account}))
            })
            .mount(&server)
            .await;
        let batch = Uuid::nil();
        let mut client = StripeInvoiceClient::new("sk_test_synthetic", false).unwrap();
        client.base = Url::parse(&format!("{}/v1/", server.uri())).unwrap();
        let client = client.on_connected_trust("acct_Trust").await.unwrap();

        for (path, step, object_id) in [
            ("customers", "customer", "cus_synthetic"),
            ("invoices", "invoice", "in_synthetic"),
            ("invoiceitems", "item/0", "ii_synthetic"),
        ] {
            let mut mock = Mock::given(matchers::method("POST"))
                .and(matchers::header("Stripe-Account", "acct_Trust"))
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
                mock = mock.and(matchers::body_string_contains("auto_advance=false"));
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
            .and(matchers::header("Stripe-Account", "acct_Trust"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "in_synthetic", "livemode": false, "auto_advance": false,
                "status": "draft", "currency": "usd", "total": 1234
            })))
            .expect(1)
            .mount(&server)
            .await;
        let draft = client
            .create_trust_draft(
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
            )
            .await
            .unwrap();
        assert_eq!(draft.0, "in_synthetic");
    }
}

/// The server-owned subscription offer presented by hosted Checkout.
pub struct SubscriptionCheckout<'a> {
    pub purchase_id: Uuid,
    pub email: &'a str,
    pub name: &'a str,
    pub amount_cents: i64,
    pub interval: &'a str,
    pub terms: &'a str,
    pub site_url: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct CheckoutSession {
    pub id: String,
    pub livemode: bool,
    pub status: String,
    pub payment_status: String,
    pub client_reference_id: String,
    pub customer: Option<String>,
    pub subscription: Option<String>,
    pub url: Option<String>,
    pub consent: Option<CheckoutConsent>,
}

#[derive(Debug, Deserialize)]
pub struct CheckoutConsent {
    pub terms_of_service: Option<String>,
}

impl CheckoutSession {
    #[must_use]
    pub fn payment_url(&self) -> Option<&str> {
        self.url
            .as_deref()
            .filter(|url| trusted_stripe_url(url, "checkout.stripe.com"))
    }
}

#[derive(Debug, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub livemode: bool,
    pub customer: String,
    pub status: String,
    pub cancel_at_period_end: bool,
    pub items: SubscriptionItems,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionItems {
    pub data: Vec<SubscriptionItem>,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionItem {
    pub current_period_end: i64,
    pub price: SubscriptionPrice,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionPrice {
    pub unit_amount: Option<i64>,
    pub currency: String,
    pub recurring: SubscriptionInterval,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionInterval {
    pub interval: String,
    pub interval_count: u32,
}

impl StripeInvoiceClient<'_> {
    /// Host the payment form at Stripe; the application never handles card data.
    pub async fn create_subscription_checkout(
        &self,
        offer: &SubscriptionCheckout<'_>,
    ) -> Result<CheckoutSession, BillingError> {
        if offer.amount_cents <= 0
            || !matches!(offer.interval, "month" | "year")
            || offer.terms.trim().is_empty()
        {
            return Err(provider_error("subscription terms or price missing"));
        }
        let customer: StripeObject = self
            .call(
                Method::POST,
                "customers",
                Some(&format!("{}/customer", offer.purchase_id)),
                &[
                    ("email", offer.email),
                    ("invoice_settings[footer]", offer.terms),
                    (
                        "metadata[navigator_purchase_id]",
                        &offer.purchase_id.to_string(),
                    ),
                ],
            )
            .await?;
        if customer.livemode != self.live_mode() || !stripe_identifier(&customer.id, "cus_") {
            return Err(provider_error("Stripe returned an invalid customer"));
        }
        let site = offer.site_url.trim_end_matches('/');
        let fields = [
            ("mode", "subscription".to_string()),
            ("client_reference_id", offer.purchase_id.to_string()),
            ("customer", customer.id),
            ("line_items[0][quantity]", "1".to_string()),
            ("line_items[0][price_data][currency]", "usd".to_string()),
            ("line_items[0][price_data][unit_amount]", offer.amount_cents.to_string()),
            ("line_items[0][price_data][recurring][interval]", offer.interval.to_string()),
            ("line_items[0][price_data][product_data][name]", offer.name.to_string()),
            ("custom_text[submit][message]", offer.terms.to_string()),
            ("consent_collection[terms_of_service]", "required".to_string()),
            ("custom_text[terms_of_service_acceptance][message]", format!("I agree to the [terms]({site}/terms), including the renewal and cancellation terms shown above.")),
            ("success_url", format!("{site}/purchases")),
            ("cancel_url", format!("{site}/purchases")),
            ("subscription_data[metadata][navigator_purchase_id]", offer.purchase_id.to_string()),
        ];
        let borrowed: Vec<_> = fields
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let session: CheckoutSession = self
            .call(
                Method::POST,
                "checkout/sessions",
                Some(&format!("{}/checkout", offer.purchase_id)),
                &borrowed,
            )
            .await?;
        if session.livemode != self.live_mode()
            || session.client_reference_id != offer.purchase_id.to_string()
            || !session.id.starts_with("cs_")
            || session.status != "open"
            || !session
                .url
                .as_deref()
                .is_some_and(|url| trusted_stripe_url(url, "checkout.stripe.com"))
        {
            return Err(provider_error("Stripe returned an invalid checkout"));
        }
        Ok(session)
    }

    pub async fn checkout_session(&self, id: &str) -> Result<CheckoutSession, BillingError> {
        if !stripe_identifier(id, "cs_") {
            return Err(provider_error("invalid checkout identity"));
        }
        let session: CheckoutSession = self
            .call(Method::GET, &format!("checkout/sessions/{id}"), None, &[])
            .await?;
        if session.id != id || session.livemode != self.live_mode() {
            return Err(provider_error("checkout identity mismatch"));
        }
        Ok(session)
    }

    pub async fn subscription(&self, id: &str) -> Result<Subscription, BillingError> {
        if !stripe_identifier(id, "sub_") {
            return Err(provider_error("invalid subscription identity"));
        }
        let subscription: Subscription = self
            .call(Method::GET, &format!("subscriptions/{id}"), None, &[])
            .await?;
        self.check_subscription(id, subscription)
    }

    /// Stop future renewals without ending the already paid period.
    pub async fn cancel_subscription(
        &self,
        purchase: Uuid,
        id: &str,
    ) -> Result<Subscription, BillingError> {
        if !stripe_identifier(id, "sub_") {
            return Err(provider_error("invalid subscription identity"));
        }
        let subscription: Subscription = self
            .call(
                Method::POST,
                &format!("subscriptions/{id}"),
                Some(&format!("{purchase}/cancel")),
                &[("cancel_at_period_end", "true")],
            )
            .await?;
        let subscription = self.check_subscription(id, subscription)?;
        if !subscription.cancel_at_period_end {
            return Err(provider_error("subscription cancellation not confirmed"));
        }
        Ok(subscription)
    }

    fn live_mode(&self) -> bool {
        self.key.starts_with("sk_live_") || self.key.starts_with("rk_live_")
    }

    fn check_subscription(
        &self,
        id: &str,
        subscription: Subscription,
    ) -> Result<Subscription, BillingError> {
        if subscription.id != id
            || subscription.livemode != self.live_mode()
            || !stripe_identifier(&subscription.customer, "cus_")
        {
            return Err(provider_error("subscription identity mismatch"));
        }
        Ok(subscription)
    }
}

fn stripe_identifier(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn trusted_stripe_url(value: &str, host: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some(host)
            && url.username().is_empty()
            && url.password().is_none()
    })
}

#[cfg(test)]
mod subscription_tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn session(purchase: Uuid) -> serde_json::Value {
        json!({"id":"cs_test_checkout", "livemode":false, "status":"open", "payment_status":"unpaid",
            "client_reference_id":purchase.to_string(), "customer":null, "subscription":null,
            "url":"https://checkout.stripe.com/c/pay/cs_test_checkout"})
    }

    async fn mount_customer(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/v1/customers"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id":"cus_synthetic", "livemode":false})),
            )
            .mount(server)
            .await;
    }

    fn subscription() -> serde_json::Value {
        json!({"id":"sub_synthetic", "livemode":false, "customer":"cus_synthetic", "status":"active",
            "cancel_at_period_end":true, "items":{"data":[{"current_period_end":1_900_000_000,
            "price":{"unit_amount":2900, "currency":"usd", "recurring":{"interval":"month","interval_count":1}}}]}})
    }

    #[tokio::test]
    async fn hosted_checkout_requires_terms_and_places_them_beside_payment_with_explicit_consent() {
        let server = MockServer::start().await;
        let purchase = Uuid::now_v7();
        mount_customer(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/checkout/sessions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(session(purchase)))
            .expect(1)
            .mount(&server)
            .await;
        let mut stripe = StripeInvoiceClient::new("sk_test_synthetic", false).unwrap();
        stripe.base = Url::parse(&format!("{}/v1/", server.uri())).unwrap();
        let mut offer = SubscriptionCheckout {
            purchase_id: purchase,
            email: "client@example.com",
            name: "Monthly plan",
            amount_cents: 2900,
            interval: "month",
            terms: "Renews at $29 monthly. Cancel renewal online before the next charge.",
            site_url: "https://app.example",
        };
        assert!(stripe
            .create_subscription_checkout(&offer)
            .await
            .unwrap()
            .payment_url()
            .is_some());
        let requests = server.received_requests().await.unwrap();
        let checkout_request = requests
            .iter()
            .find(|request| request.url.path().ends_with("/checkout/sessions"))
            .unwrap();
        let body = std::str::from_utf8(&checkout_request.body).unwrap();
        let url = Url::parse(&format!("https://example.com/?{body}")).unwrap();
        let fields: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(fields["mode"], "subscription");
        assert_eq!(fields["line_items[0][price_data][unit_amount]"], "2900");
        assert_eq!(
            fields["line_items[0][price_data][recurring][interval]"],
            "month"
        );
        assert_eq!(fields["custom_text[submit][message]"], offer.terms);
        assert_eq!(fields["consent_collection[terms_of_service]"], "required");
        assert_eq!(fields["client_reference_id"], purchase.to_string());
        assert!(checkout_request.headers.contains_key("idempotency-key"));
        let customer_request = requests
            .iter()
            .find(|request| request.url.path().ends_with("/customers"))
            .unwrap();
        let customer_body = std::str::from_utf8(&customer_request.body).unwrap();
        let customer_fields: std::collections::HashMap<_, _> =
            Url::parse(&format!("https://example.com/?{customer_body}"))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
        assert_eq!(customer_fields["invoice_settings[footer]"], offer.terms);
        assert!(!body.contains("card"));
        offer.terms = " ";
        assert!(stripe.create_subscription_checkout(&offer).await.is_err());
        offer.terms = "Terms";
        offer.interval = "day";
        assert!(stripe.create_subscription_checkout(&offer).await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn checkout_refuses_a_foreign_reference_or_offsite_payment_url() {
        for invalid in ["reference", "url", "mode"] {
            let server = MockServer::start().await;
            let purchase = Uuid::now_v7();
            mount_customer(&server).await;
            let mut result = session(purchase);
            match invalid {
                "reference" => result["client_reference_id"] = json!(Uuid::now_v7().to_string()),
                "url" => result["url"] = json!("https://checkout.stripe.com.example/steal"),
                _ => result["livemode"] = json!(true),
            }
            Mock::given(method("POST"))
                .and(path("/v1/checkout/sessions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(result))
                .mount(&server)
                .await;
            let mut stripe = StripeInvoiceClient::new("sk_test_synthetic", false).unwrap();
            stripe.base = Url::parse(&format!("{}/v1/", server.uri())).unwrap();
            assert!(stripe
                .create_subscription_checkout(&SubscriptionCheckout {
                    purchase_id: purchase,
                    email: "client@example.com",
                    name: "Plan",
                    amount_cents: 2900,
                    interval: "month",
                    terms: "Terms",
                    site_url: "https://app.example"
                })
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn cancel_stops_renewal_at_period_end_and_rejects_unconfirmed_or_foreign_results() {
        for invalid in [None, Some("confirmation"), Some("identity"), Some("mode")] {
            let server = MockServer::start().await;
            let mut result = subscription();
            match invalid {
                Some("confirmation") => result["cancel_at_period_end"] = json!(false),
                Some("identity") => result["id"] = json!("sub_other"),
                Some("mode") => result["livemode"] = json!(true),
                _ => {}
            }
            Mock::given(method("POST"))
                .and(path("/v1/subscriptions/sub_synthetic"))
                .respond_with(ResponseTemplate::new(200).set_body_json(result))
                .expect(1)
                .mount(&server)
                .await;
            let mut stripe = StripeInvoiceClient::new("sk_test_synthetic", false).unwrap();
            stripe.base = Url::parse(&format!("{}/v1/", server.uri())).unwrap();
            let response = stripe
                .cancel_subscription(Uuid::now_v7(), "sub_synthetic")
                .await;
            assert_eq!(response.is_ok(), invalid.is_none());
            let requests = server.received_requests().await.unwrap();
            assert_eq!(
                std::str::from_utf8(&requests[0].body).unwrap(),
                "cancel_at_period_end=true"
            );
        }
    }

    #[tokio::test]
    async fn provider_identifiers_cannot_escape_the_subscription_path() {
        let stripe = StripeInvoiceClient::new("sk_test_synthetic", false).unwrap();
        assert!(stripe.subscription("sub_../customers").await.is_err());
        assert!(stripe
            .checkout_session("cs_?expand=customer")
            .await
            .is_err());
        assert!(stripe
            .cancel_subscription(Uuid::now_v7(), "sub_../other")
            .await
            .is_err());
    }
}

#[cfg(test)]
#[path = "stripe_sandbox.rs"]
mod sandbox;

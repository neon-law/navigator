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
    #[serde(default)]
    subtotal: i64,
}

/// A lawyer-composed invoice for [`StripeInvoiceClient::create_composed_draft`].
pub struct ComposedDraft<'a> {
    pub request: &'a InvoiceRequest,
    pub customer: &'a str,
    pub promotion_code: Option<&'a str>,
    pub memo: Option<&'a str>,
}

fn provider_error(message: &str) -> BillingError {
    BillingError::Provider(message.to_owned())
}

/// A sandbox client may talk to a local Stripe stand-in (an end-to-end test
/// or `stripe-mock`). Live keys always reach Stripe itself, and the override
/// names only a loopback host, so a key is never sent elsewhere.
const API_BASE_VAR: &str = "NAVIGATOR_STRIPE_API_BASE";

fn api_base(live: bool, local: Option<&str>) -> Result<Url, BillingError> {
    let invalid = || provider_error("Stripe API configuration invalid");
    match local.filter(|_| !live) {
        Some(local) => {
            let url = Url::parse(local).map_err(|_| invalid())?;
            let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            if !loopback || url.scheme() != "http" {
                return Err(invalid());
            }
            url.join("v1/").map_err(|_| invalid())
        }
        None => Url::parse("https://api.stripe.com/v1/").map_err(|_| invalid()),
    }
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
            base: api_base(live, std::env::var(API_BASE_VAR).ok().as_deref())?,
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

    /// A lawyer-composed draft (LAW-506): its lines (a negative line is a
    /// discount, write-off, or credit with its reason), an optional
    /// promotion code, and the memo the client sees. Nothing is sent.
    pub async fn create_composed_draft(
        &self,
        key: Uuid,
        draft: &ComposedDraft<'_>,
    ) -> Result<InvoiceId, BillingError> {
        let promotion = match draft.promotion_code {
            Some(code) => Some(self.promotion_code(code).await?),
            None => None,
        };
        let due = draft
            .request
            .due_date
            .ok_or_else(|| provider_error("invoice due date missing"))?
            .and_hms_opt(23, 59, 59)
            .ok_or_else(|| provider_error("invoice due date invalid"))?
            .and_utc()
            .timestamp()
            .to_string();
        let mut extra = vec![("metadata[navigator_invoice_id]", key.to_string())];
        if let Some(memo) = draft.memo {
            extra.push(("description", memo.to_owned()));
        }
        if let Some(promotion) = promotion {
            extra.push(("discounts[0][promotion_code]", promotion));
        }
        self.draft_with(key, draft.request, draft.customer, &due, &extra)
            .await
    }

    /// The active promotion code's id for a code the client was given.
    async fn promotion_code(&self, code: &str) -> Result<String, BillingError> {
        #[derive(Deserialize)]
        struct Codes {
            data: Vec<StripeObject>,
        }
        if code.is_empty()
            || code.len() > 64
            || !code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(provider_error("coupon code invalid"));
        }
        let codes: Codes = self
            .call(
                Method::GET,
                &format!("promotion_codes?code={code}&active=true&limit=1"),
                None,
                &[],
            )
            .await?;
        codes
            .data
            .into_iter()
            .find(|found| {
                found.livemode == self.live_mode() && stripe_identifier(&found.id, "promo_")
            })
            .map(|found| found.id)
            .ok_or_else(|| provider_error("no active coupon has that code"))
    }

    /// Finalize a reviewed draft and send it. A zero-balance invoice is
    /// finalized as a record (Stripe marks it paid) and never sent.
    pub async fn finalize_and_send(
        &self,
        key: Uuid,
        id: &str,
    ) -> Result<SentInvoice, BillingError> {
        if !stripe_identifier(id, "in_") {
            return Err(provider_error("invalid invoice identity"));
        }
        let finalized: SentInvoice = self
            .call(
                Method::POST,
                &format!("invoices/{id}/finalize"),
                Some(&format!("{key}/finalize")),
                &[("auto_advance", "false")],
            )
            .await?;
        let sent = if finalized.total == 0 {
            finalized
        } else {
            self.call(
                Method::POST,
                &format!("invoices/{id}/send"),
                Some(&format!("{key}/send")),
                &[],
            )
            .await?
        };
        if sent.id != id || sent.livemode != self.live_mode() || sent.status == "draft" {
            return Err(provider_error("Stripe invoice does not match the request"));
        }
        Ok(sent)
    }

    /// Void a sent invoice, or mark it uncollectible, under `key`.
    pub async fn close_invoice(
        &self,
        key: Uuid,
        id: &str,
        uncollectible: bool,
    ) -> Result<(), BillingError> {
        if !stripe_identifier(id, "in_") {
            return Err(provider_error("invalid invoice identity"));
        }
        let (verb, expected) = if uncollectible {
            ("mark_uncollectible", "uncollectible")
        } else {
            ("void", "void")
        };
        let closed: SentInvoice = self
            .call(
                Method::POST,
                &format!("invoices/{id}/{verb}"),
                Some(&format!("{key}/{verb}")),
                &[],
            )
            .await?;
        if closed.id != id || closed.status != expected {
            return Err(provider_error("Stripe invoice does not match the request"));
        }
        Ok(())
    }

    async fn create_draft_for_customer(
        &self,
        batch: Uuid,
        request: &InvoiceRequest,
        customer: &str,
        due: &str,
    ) -> Result<InvoiceId, BillingError> {
        let extra = [
            ("description", request.reference.clone()),
            ("metadata[navigator_batch_id]", batch.to_string()),
        ];
        self.draft_with(batch, request, customer, due, &extra).await
    }

    async fn draft_with(
        &self,
        batch: Uuid,
        request: &InvoiceRequest,
        customer: &str,
        due: &str,
        extra: &[(&str, String)],
    ) -> Result<InvoiceId, BillingError> {
        let mut fields = vec![
            ("customer", customer),
            ("auto_advance", "false"),
            ("collection_method", "send_invoice"),
            ("due_date", due),
            ("pending_invoice_items_behavior", "exclude"),
        ];
        fields.extend(extra.iter().map(|(name, value)| (*name, value.as_str())));
        let discounted = extra.iter().any(|(name, _)| name.starts_with("discounts["));
        let invoice: StripeObject = self
            .call(
                Method::POST,
                "invoices",
                Some(&format!("{batch}/invoice")),
                &fields,
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
            || (if discounted {
                draft.subtotal
            } else {
                draft.total
            }) != total
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
        assert_eq!(
            api_base(false, Some("http://127.0.0.1:4010"))
                .unwrap()
                .as_str(),
            "http://127.0.0.1:4010/v1/"
        );
        assert_eq!(
            api_base(true, Some("http://127.0.0.1:4010"))
                .unwrap()
                .as_str(),
            "https://api.stripe.com/v1/"
        );
        assert!(api_base(false, Some("https://stripe.example/")).is_err());
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
        let customer = client
            .ensure_customer(None, batch, "Synthetic Client", "synthetic@example.test")
            .await
            .unwrap();
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
                &customer,
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

/// Who a one-off invoice bills: a Customer Stripe already holds, or a new
/// one created under the invoice's idempotency key.
pub enum InvoiceCustomer<'a> {
    Existing(&'a str),
    New { name: &'a str, email: &'a str },
}

/// A one-off invoice the payer asked for or owes outright: a self-serve
/// Purchase or a month's Legal Sandbox overage.
pub struct OneOffInvoice<'a> {
    /// The stable key every write uses, so a retry finds the same objects.
    pub key: Uuid,
    pub customer: InvoiceCustomer<'a>,
    pub description: &'a str,
    pub amount_cents: i64,
    pub days_until_due: u32,
    /// `metadata[<name>]` naming the Navigator record the invoice answers for.
    pub metadata: (&'a str, String),
}

/// A finalized, sent invoice and what Stripe reports about its payment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SentInvoice {
    pub id: String,
    pub livemode: bool,
    pub status: String,
    pub currency: String,
    pub total: i64,
    #[serde(default)]
    pub amount_paid: i64,
    #[serde(default)]
    pub hosted_invoice_url: Option<String>,
}

impl SentInvoice {
    /// Paid in full in USD: the only state that turns a Purchase on.
    #[must_use]
    pub fn paid(&self, amount_cents: i64) -> bool {
        self.status == "paid"
            && self.currency == "usd"
            && self.total == amount_cents
            && self.amount_paid == amount_cents
    }
}

impl StripeInvoiceClient<'_> {
    /// Create, finalize, and send one invoice. The hosted invoice page is the
    /// payer's pay link. Every write carries `key`, so a retry within Stripe's
    /// idempotency window converges on the same invoice.
    pub async fn send_one_off_invoice(
        &self,
        invoice: &OneOffInvoice<'_>,
    ) -> Result<SentInvoice, BillingError> {
        if invoice.amount_cents <= 0 || invoice.description.trim().is_empty() {
            return Err(provider_error("invoice amount or description missing"));
        }
        let key = invoice.key;
        let customer = match invoice.customer {
            InvoiceCustomer::Existing(id) => {
                if !stripe_identifier(id, "cus_") {
                    return Err(provider_error("invalid customer identity"));
                }
                id.to_owned()
            }
            InvoiceCustomer::New { name, email } => {
                let customer: StripeObject = self
                    .call(
                        Method::POST,
                        "customers",
                        Some(&format!("{key}/customer")),
                        &[("name", name), ("email", email)],
                    )
                    .await?;
                if customer.livemode != self.live_mode() || !stripe_identifier(&customer.id, "cus_")
                {
                    return Err(provider_error("Stripe returned an invalid customer"));
                }
                customer.id
            }
        };
        let (metadata_name, metadata_value) = &invoice.metadata;
        let metadata_field = format!("metadata[{metadata_name}]");
        let days = invoice.days_until_due.to_string();
        let draft: StripeObject = self
            .call(
                Method::POST,
                "invoices",
                Some(&format!("{key}/invoice")),
                &[
                    ("customer", &customer),
                    ("auto_advance", "false"),
                    ("collection_method", "send_invoice"),
                    ("days_until_due", &days),
                    ("description", invoice.description),
                    ("pending_invoice_items_behavior", "exclude"),
                    (&metadata_field, metadata_value),
                ],
            )
            .await?;
        if draft.livemode != self.live_mode() || !stripe_identifier(&draft.id, "in_") {
            return Err(provider_error("Stripe returned an invalid invoice"));
        }
        let amount = invoice.amount_cents.to_string();
        let _: StripeObject = self
            .call(
                Method::POST,
                "invoiceitems",
                Some(&format!("{key}/item/0")),
                &[
                    ("customer", &customer),
                    ("invoice", &draft.id),
                    ("currency", "usd"),
                    ("amount", &amount),
                    ("description", invoice.description),
                ],
            )
            .await?;
        let _: SentInvoice = self
            .call(
                Method::POST,
                &format!("invoices/{}/finalize", draft.id),
                Some(&format!("{key}/finalize")),
                &[("auto_advance", "false")],
            )
            .await?;
        let sent: SentInvoice = self
            .call(
                Method::POST,
                &format!("invoices/{}/send", draft.id),
                Some(&format!("{key}/send")),
                &[],
            )
            .await?;
        self.check_sent(&draft.id, invoice.amount_cents, sent)
    }

    /// Read one invoice back, for its payment status.
    pub async fn invoice_state(
        &self,
        id: &str,
        amount_cents: i64,
    ) -> Result<SentInvoice, BillingError> {
        if !stripe_identifier(id, "in_") {
            return Err(provider_error("invalid invoice identity"));
        }
        let invoice: SentInvoice = self
            .call(Method::GET, &format!("invoices/{id}"), None, &[])
            .await?;
        self.check_sent(id, amount_cents, invoice)
    }

    fn check_sent(
        &self,
        id: &str,
        amount_cents: i64,
        invoice: SentInvoice,
    ) -> Result<SentInvoice, BillingError> {
        if invoice.id != id
            || invoice.livemode != self.live_mode()
            || invoice.currency != "usd"
            || invoice.total != amount_cents
            || invoice.status == "draft"
            || invoice
                .hosted_invoice_url
                .as_deref()
                .is_some_and(|url| !trusted_stripe_url(url, "invoice.stripe.com"))
        {
            return Err(provider_error("Stripe invoice does not match the request"));
        }
        Ok(invoice)
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

#[cfg(test)]
mod one_off_tests {
    use super::*;
    use serde_json::json;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    fn sent(status: &str, paid: i64) -> serde_json::Value {
        json!({"id":"in_synthetic", "livemode":false, "status":status, "currency":"usd",
            "total":10000, "amount_paid":paid,
            "hosted_invoice_url":"https://invoice.stripe.com/i/synthetic"})
    }

    fn client(server: &MockServer) -> StripeInvoiceClient<'static> {
        let mut client = StripeInvoiceClient::new("sk_test_synthetic", false).unwrap();
        client.base = Url::parse(&format!("{}/v1/", server.uri())).unwrap();
        client
    }

    #[tokio::test]
    async fn a_one_off_invoice_is_created_finalized_and_sent_under_one_stable_key() {
        let server = MockServer::start().await;
        let key = Uuid::nil();
        for (route, step, body) in [
            (
                "/v1/customers",
                "customer",
                json!({"id":"cus_synthetic","livemode":false}),
            ),
            (
                "/v1/invoices",
                "invoice",
                json!({"id":"in_synthetic","livemode":false}),
            ),
            (
                "/v1/invoiceitems",
                "item/0",
                json!({"id":"ii_synthetic","livemode":false}),
            ),
            (
                "/v1/invoices/in_synthetic/finalize",
                "finalize",
                sent("open", 0),
            ),
            ("/v1/invoices/in_synthetic/send", "send", sent("open", 0)),
        ] {
            let mut mock = Mock::given(matchers::method("POST"))
                .and(matchers::path(route))
                .and(matchers::header("Idempotency-Key", format!("{key}/{step}")));
            if step == "invoice" {
                mock = mock
                    .and(matchers::body_string_contains(
                        "collection_method=send_invoice",
                    ))
                    .and(matchers::body_string_contains("days_until_due=14"))
                    .and(matchers::body_string_contains("navigator_purchase_id"));
            }
            if step == "item/0" {
                mock = mock.and(matchers::body_string_contains("amount=10000"));
            }
            mock.respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
        }
        let invoice = client(&server)
            .send_one_off_invoice(&OneOffInvoice {
                key,
                customer: InvoiceCustomer::New {
                    name: "Synthetic Client",
                    email: "client@example.com",
                },
                description: "Consultation",
                amount_cents: 10_000,
                days_until_due: 14,
                metadata: ("navigator_purchase_id", key.to_string()),
            })
            .await
            .unwrap();
        assert_eq!(
            invoice.hosted_invoice_url.as_deref(),
            Some("https://invoice.stripe.com/i/synthetic")
        );
        assert!(!invoice.paid(10_000));
    }

    #[tokio::test]
    async fn only_a_full_usd_payment_of_the_requested_total_reads_as_paid() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/v1/invoices/in_synthetic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sent("paid", 10_000)))
            .mount(&server)
            .await;
        let stripe = client(&server);
        assert!(stripe
            .invoice_state("in_synthetic", 10_000)
            .await
            .unwrap()
            .paid(10_000));
        assert!(stripe.invoice_state("in_synthetic", 9_000).await.is_err());
        assert!(stripe
            .invoice_state("not_an_invoice", 10_000)
            .await
            .is_err());
        let mut partial: SentInvoice = serde_json::from_value(sent("open", 5_000)).unwrap();
        assert!(!partial.paid(10_000));
        partial.status = "paid".into();
        assert!(!partial.paid(10_000), "a short payment never activates");
        let foreign = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id":"in_other", "livemode":false, "status":"paid", "currency":"usd",
                "total":10000, "amount_paid":10000, "hosted_invoice_url":"https://evil.example/i"})))
            .mount(&foreign)
            .await;
        assert!(client(&foreign)
            .invoice_state("in_other", 10_000)
            .await
            .is_err());
    }
}

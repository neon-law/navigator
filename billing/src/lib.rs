//! Billing-provider seam: the Firm's books in Xero.
//!
//! Xero is the Firm's books, not an invoice issuer. [`BillingProvider`]
//! reads the pooled IOLTA trust bank accounts and their transactions for
//! the nightly trust mirror, and resolves (find-or-creates) billing
//! contacts for the billing canary. Matter invoices, Purchases, and
//! Sandbox overage are issued through Stripe ([`stripe`]), which takes
//! [`InvoiceRequest`] and [`InvoiceLine`].
//!
//! [`XeroBillingProvider`] is the Xero Accounting API adapter.
//! [`StubBillingProvider`] records contact resolves and serves canned trust
//! reads for development fixtures and tests; [`DisabledBillingProvider`]
//! refuses everything where no Xero connection is configured.
//!
//! Lives in its own crate (not `web`) so the worker-side billing
//! workflows in `billing-workflows` can reach the same provider seam —
//! `web` re-exports it as `portal::billing` / `portal::xero_auth`.

pub mod gcp_cost;
pub mod gcp_cost_daily;
pub mod stripe;
pub mod xero_auth;

pub use gcp_cost::{
    adc_token_provider, billing_account_from_table, billing_export_tables, format_cost_sql,
    parse_cost_rows, BillingClient, CostReport, CostRow, ProjectCost, StaticToken, TokenProvider,
};

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::xero_auth::XeroClientCredentials;

/// Opaque identifier the invoice issuer (Stripe) returns for a created
/// invoice. Used to correlate later events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceId(pub String);

/// Opaque identifier for a billing contact (Xero's `ContactID` GUID).
/// Resolved once per billed party via find-or-create on email; the same
/// email always returns the same contact, never duplicated, so resolution
/// is idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactId(pub String);

/// One bank account as listed from the provider, read-only. The firm's
/// pooled IOLTA account for a state is one of these; which state it serves
/// is a Navigator mapping (`store::iolta_accounts`), not something Xero
/// knows. Money is minor units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustBankAccount {
    /// Xero `AccountID` (a GUID) — the stable identity.
    pub account_id: String,
    /// Xero's short chart-of-accounts `Code` (`"090"`), when set.
    pub code: Option<String>,
    pub name: String,
    pub currency: String,
    /// The balance Xero reports for the account, in minor units.
    pub balance_cents: i64,
}

/// Which way money moved on a bank account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustTransactionKind {
    /// Money into the account — a client's deposit into trust.
    Receive,
    /// Money out of the account — either the pooled IOLTA → operating
    /// withdrawal that settles invoices, or a refund of unearned funds back
    /// to a client. Which one it is, is told by whether the lines name
    /// invoices.
    Spend,
}

/// One line on a bank transaction. On the pooled withdrawal each line is an
/// allocation: how much of the one bank transfer settles which invoice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustTransactionLine {
    pub description: String,
    pub amount_cents: i64,
    /// The invoice this line settles, as the line's own reference names it.
    /// `None` on a line that settles no invoice — a refund's line, or an
    /// unlabelled one.
    pub invoice_reference: Option<String>,
}

/// One bank transaction on a trust account, read-only. Navigator mirrors
/// these; the firm's bookkeeper records them in Xero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustBankTransaction {
    /// Xero `BankTransactionID` — the idempotency key the mirror posts on.
    pub transaction_id: String,
    pub kind: TrustTransactionKind,
    /// The bank account it moved on; matched against the mirrored pooled
    /// account for a state.
    pub account_id: String,
    /// The transaction-level `Reference` (`Matter <project uuid or code>`).
    pub reference: String,
    /// Total moved, minor units.
    pub amount_cents: i64,
    pub currency: String,
    pub occurred_at: DateTime<Utc>,
    pub line_items: Vec<TrustTransactionLine>,
}

/// One line on an invoice. Money is carried in **minor units (cents)** —
/// never a float — and rendered to a decimal string only at the wire
/// boundary. Quantity is whole units (a flat-fee matter bills quantity 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceLine {
    pub description: String,
    pub quantity: u32,
    pub unit_amount_cents: i64,
    /// Xero chart-of-accounts code the revenue posts to (e.g. `"200"`).
    pub account_code: String,
}

/// The invoice to raise for a matter: who is billed and the line items.
/// `reference` is the human-facing matter reference shown on the invoice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceRequest {
    /// Resolved accounting contact, when the caller has one. Absent on
    /// older journaled requests; Stripe bills by name and email.
    #[serde(default)]
    pub contact_id: Option<ContactId>,
    /// The billed party, as Stripe shows it on the invoice.
    pub contact_name: String,
    pub contact_email: String,
    pub reference: String,
    pub line_items: Vec<InvoiceLine>,
    /// Explicit issue date when the caller freezes one.
    #[serde(default)]
    pub issued_date: Option<NaiveDate>,
    /// When payment is due, when the caller sets one.
    #[serde(default)]
    pub due_date: Option<NaiveDate>,
}

/// The billed party to resolve (find-or-create) in the accounting system
/// before an invoice is raised against it. `email` is the **match key** —
/// the payer's stable identity that find-or-create looks up on — and
/// `name` is the display name, set as Xero's required unique `Name` when
/// the contact is first created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactRequest {
    pub name: String,
    pub email: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BillingError {
    #[error("provider error: {0}")]
    Provider(String),
}

#[async_trait]
pub trait BillingProvider: Send + Sync {
    /// Resolve the billed party to a provider contact id, creating the
    /// contact when it does not yet exist. Idempotent: a contact with the
    /// same email is returned, never duplicated.
    async fn ensure_contact(&self, request: &ContactRequest) -> Result<ContactId, BillingError>;

    /// List the provider's bank accounts. Read-only, and read for exactly
    /// one purpose: refreshing the mirrored balance of the pooled IOLTA
    /// account each state's matters draw on. Navigator never opens, closes,
    /// or writes to an account — Xero is the books.
    async fn list_bank_accounts(&self) -> Result<Vec<TrustBankAccount>, BillingError>;

    /// List bank transactions — the deposits into trust, the refunds out of
    /// it, and the pooled withdrawal that settles invoices. Read-only: the
    /// firm posts these in Xero and Navigator mirrors what it finds.
    async fn list_trust_transactions(&self) -> Result<Vec<TrustBankTransaction>, BillingError>;
}

/// Unavailable accounting provider. It neither contacts Xero nor fabricates invoices.
#[derive(Default)]
pub struct DisabledBillingProvider;

fn billing_disabled() -> BillingError {
    BillingError::Provider("billing automation is disabled".into())
}

#[async_trait]
impl BillingProvider for DisabledBillingProvider {
    async fn ensure_contact(&self, _: &ContactRequest) -> Result<ContactId, BillingError> {
        Err(billing_disabled())
    }
    async fn list_bank_accounts(&self) -> Result<Vec<TrustBankAccount>, BillingError> {
        Err(billing_disabled())
    }
    async fn list_trust_transactions(&self) -> Result<Vec<TrustBankTransaction>, BillingError> {
        Err(billing_disabled())
    }
}

/// In-process stub. Records every contact resolve and serves canned
/// trust reads, empty until a test seeds them.
#[derive(Default)]
pub struct StubBillingProvider {
    contact_calls: Mutex<Vec<ContactRequest>>,
    /// Canned `list_bank_accounts` rows. Empty until a test seeds them.
    listed_bank_accounts: Mutex<Vec<TrustBankAccount>>,
    /// Canned `list_trust_transactions` rows. Empty until a test seeds them.
    listed_trust_transactions: Mutex<Vec<TrustBankTransaction>>,
}

impl StubBillingProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The development-fixture provider. Synthetic matters bill through
    /// Stripe, so this is the plain stub.
    #[must_use]
    pub fn for_simulated_matters() -> Self {
        Self::default()
    }

    /// Pre-seed what `list_bank_accounts` returns, so an IOLTA mirror test
    /// can simulate the firm's pooled trust accounts in Xero.
    pub fn set_listed_bank_accounts(&self, accounts: Vec<TrustBankAccount>) {
        *self
            .listed_bank_accounts
            .lock()
            .expect("stub provider lock") = accounts;
    }

    /// Pre-seed what `list_trust_transactions` returns, so a trust-ledger
    /// test can simulate deposits, refunds, and the pooled withdrawal.
    pub fn set_listed_trust_transactions(&self, transactions: Vec<TrustBankTransaction>) {
        *self
            .listed_trust_transactions
            .lock()
            .expect("stub provider lock") = transactions;
    }

    /// Snapshot of every `ensure_contact` call so far.
    #[must_use]
    pub fn contact_calls(&self) -> Vec<ContactRequest> {
        self.contact_calls
            .lock()
            .expect("stub provider lock")
            .clone()
    }
}

#[async_trait]
impl BillingProvider for StubBillingProvider {
    async fn ensure_contact(&self, request: &ContactRequest) -> Result<ContactId, BillingError> {
        self.contact_calls
            .lock()
            .expect("stub provider lock")
            .push(request.clone());
        // Deterministic on email, mirroring the real provider's match on
        // the payer's email — a repeated resolve is stable.
        Ok(ContactId(format!("stub-contact-{}", request.email)))
    }

    async fn list_bank_accounts(&self) -> Result<Vec<TrustBankAccount>, BillingError> {
        Ok(self
            .listed_bank_accounts
            .lock()
            .expect("stub provider lock")
            .clone())
    }

    async fn list_trust_transactions(&self) -> Result<Vec<TrustBankTransaction>, BillingError> {
        Ok(self
            .listed_trust_transactions
            .lock()
            .expect("stub provider lock")
            .clone())
    }
}

/// Re-mint a token this many seconds before Xero's stated expiry, so an
/// in-flight request never carries a token that lapses mid-call.
const TOKEN_REFRESH_LEAD_SECS: u64 = 120;

/// Current unix time in seconds — the provider's runtime token cache
/// reads the real clock here.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A minted token and the unix second at/after which it must be re-minted
/// (Xero's stated expiry, less [`TOKEN_REFRESH_LEAD_SECS`]).
#[derive(Clone)]
struct CachedToken {
    token: String,
    refresh_at: u64,
}

impl CachedToken {
    /// The token if still fresh at `now` (strictly before `refresh_at`);
    /// `None` means the caller must re-mint.
    fn fresh_at(&self, now: u64) -> Option<&str> {
        (now < self.refresh_at).then_some(self.token.as_str())
    }
}

/// How [`XeroBillingProvider`] obtains the bearer for each REST call.
/// `Static` is the simple local/demo path (a `XERO_ACCESS_TOKEN` Xero
/// expires in 30m); `ClientCredentials` is the durable path that mints +
/// caches short-lived tokens, re-minting before expiry. The cache is
/// shared across clones so a cloned handle reuses an already-minted token.
#[derive(Clone)]
enum TokenSource {
    Static(String),
    ClientCredentials {
        auth: Arc<XeroClientCredentials>,
        cached: Arc<AsyncMutex<Option<CachedToken>>>,
    },
}

/// Production [`BillingProvider`] backed by the Xero Accounting REST API:
/// billing contacts and read-only trust bank accounts and transactions.
///
/// Config is `.env`-driven so OSS forks plug in their own Xero custom
/// connection (or swap the impl): `base_url` is the Accounting API base
/// (`https://api.xero.com/api.xro/2.0`), and `tenant_id` is the connected
/// organisation's id sent as `Xero-Tenant-Id`. A custom connection binds
/// to one organisation, so `tenant_id` is fixed per environment.
#[derive(Clone)]
pub struct XeroBillingProvider {
    http: reqwest::Client,
    base_url: String,
    tenant_id: String,
    token: TokenSource,
}

impl XeroBillingProvider {
    /// Build a provider that authenticates with a static bearer
    /// (`XERO_ACCESS_TOKEN`). The simple local/demo path; Xero expires
    /// the token in 30m and it is not refreshed.
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        tenant_id: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            tenant_id: tenant_id.into(),
            token: TokenSource::Static(access_token.into()),
        }
    }

    /// Build a provider that authenticates via the client-credentials
    /// grant — minting and caching short-lived tokens from `auth`,
    /// re-minting before expiry. The durable production path.
    #[must_use]
    pub fn with_client_credentials(
        base_url: impl Into<String>,
        tenant_id: impl Into<String>,
        auth: XeroClientCredentials,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            tenant_id: tenant_id.into(),
            token: TokenSource::ClientCredentials {
                auth: Arc::new(auth),
                cached: Arc::new(AsyncMutex::new(None)),
            },
        }
    }

    /// Build from the environment, returning `None` when the required
    /// vars are absent (dev / KIND, where the stub is used instead).
    ///
    /// Required: `XERO_TENANT_ID`. Optional: `XERO_BASE_URL` (defaults to
    /// the Xero Accounting API base).
    ///
    /// Prefers the client-credentials grant when
    /// [`XeroClientCredentials::from_env`] is satisfied (client id +
    /// secret present) — the self-refreshing production path — and
    /// otherwise falls back to a static `XERO_ACCESS_TOKEN`.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        let tenant_id = get("XERO_TENANT_ID")?;
        let base_url =
            get("XERO_BASE_URL").unwrap_or_else(|| "https://api.xero.com/api.xro/2.0".to_string());
        if let Some(auth) = XeroClientCredentials::from_env() {
            Some(Self::with_client_credentials(base_url, tenant_id, auth))
        } else {
            Some(Self::new(base_url, tenant_id, get("XERO_ACCESS_TOKEN")?))
        }
    }

    /// The bearer for the next REST call. `Static` returns the configured
    /// token; `ClientCredentials` returns a cached token, minting a fresh
    /// one when none is cached or the current one is within
    /// [`TOKEN_REFRESH_LEAD_SECS`] of expiry.
    async fn bearer_token(&self) -> Result<String, BillingError> {
        match &self.token {
            TokenSource::Static(t) => Ok(t.clone()),
            TokenSource::ClientCredentials { auth, cached } => {
                let now = now_unix();
                let mut guard = cached.lock().await;
                if let Some(token) = guard.as_ref().and_then(|c| c.fresh_at(now)) {
                    return Ok(token.to_string());
                }
                let minted = auth.mint().await?;
                *guard = Some(CachedToken {
                    token: minted.access_token.clone(),
                    refresh_at: now + minted.expires_in.saturating_sub(TOKEN_REFRESH_LEAD_SECS),
                });
                Ok(minted.access_token)
            }
        }
    }

    /// Build the Xero `Contacts` POST body for a find-or-create. Pure —
    /// exposed for unit-testing the request shape without a round-trip.
    #[must_use]
    pub fn build_contact_body(&self, request: &ContactRequest) -> serde_json::Value {
        serde_json::json!({
            "Name": request.name,
            "EmailAddress": request.email,
        })
    }

    /// Look a contact up by email — the payer's stable identity — returning
    /// its id when one already exists. Xero does not force email
    /// uniqueness, so if several contacts share an address we take the
    /// first; the create path still sets a Xero-unique `Name`.
    async fn find_contact_by_email(&self, email: &str) -> Result<Option<ContactId>, BillingError> {
        let url = format!("{}/Contacts", self.base_url.trim_end_matches('/'));
        // Xero's `where` is a double-quoted string match; escape any inner
        // quote so an address can't break the predicate.
        let predicate = format!("EmailAddress==\"{}\"", email.replace('"', "\\\""));
        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.bearer_token().await?)
            .header("Xero-Tenant-Id", &self.tenant_id)
            .header(reqwest::header::ACCEPT, "application/json")
            .query(&[("where", predicate)])
            .send()
            .await
            .map_err(|e| BillingError::Provider(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BillingError::Provider(format!(
                "xero responded {status}: {body}"
            )));
        }
        let parsed: ContactsResponse = resp
            .json()
            .await
            .map_err(|e| BillingError::Provider(e.to_string()))?;
        Ok(parsed
            .contacts
            .into_iter()
            .next()
            .map(|c| ContactId(c.contact_id)))
    }

    /// Create a new Xero contact and return its id.
    async fn create_contact(&self, request: &ContactRequest) -> Result<ContactId, BillingError> {
        let url = format!("{}/Contacts", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .bearer_auth(self.bearer_token().await?)
            .header("Xero-Tenant-Id", &self.tenant_id)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&self.build_contact_body(request))
            .send()
            .await
            .map_err(|e| BillingError::Provider(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BillingError::Provider(format!(
                "xero responded {status}: {body}"
            )));
        }
        let parsed: ContactsResponse = resp
            .json()
            .await
            .map_err(|e| BillingError::Provider(e.to_string()))?;
        parsed
            .contacts
            .into_iter()
            .next()
            .map(|c| ContactId(c.contact_id))
            .ok_or_else(|| BillingError::Provider("xero returned no contact".to_string()))
    }
}

/// Xero's contact lookup/create response — the Accounting API wraps
/// results in a top-level `Contacts` array (empty when nothing matched).
#[derive(Deserialize)]
struct ContactsResponse {
    #[serde(rename = "Contacts", default)]
    contacts: Vec<ContactSummary>,
}

#[derive(Deserialize)]
struct ContactSummary {
    #[serde(rename = "ContactID")]
    contact_id: String,
}

#[async_trait]
impl BillingProvider for XeroBillingProvider {
    async fn ensure_contact(&self, request: &ContactRequest) -> Result<ContactId, BillingError> {
        if let Some(existing) = self.find_contact_by_email(&request.email).await? {
            return Ok(existing);
        }
        self.create_contact(request).await
    }

    async fn list_bank_accounts(&self) -> Result<Vec<TrustBankAccount>, BillingError> {
        let url = format!("{}/Accounts", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.bearer_token().await?)
            .header("Xero-Tenant-Id", &self.tenant_id)
            .header(reqwest::header::ACCEPT, "application/json")
            .query(&[("where", r#"Type=="BANK""#)])
            .send()
            .await
            .map_err(|e| BillingError::Provider(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BillingError::Provider(format!(
                "xero responded {status}: {body}"
            )));
        }
        let parsed: AccountsResponse = resp
            .json()
            .await
            .map_err(|e| BillingError::Provider(e.to_string()))?;
        Ok(parsed
            .accounts
            .into_iter()
            .map(AccountSummary::into_bank_account)
            .collect())
    }

    async fn list_trust_transactions(&self) -> Result<Vec<TrustBankTransaction>, BillingError> {
        let url = format!("{}/BankTransactions", self.base_url.trim_end_matches('/'));
        let mut transactions = Vec::new();
        let mut page = 1_u32;
        loop {
            let page_param = page.to_string();
            let resp = self
                .http
                .get(&url)
                .bearer_auth(self.bearer_token().await?)
                .header("Xero-Tenant-Id", &self.tenant_id)
                .header(reqwest::header::ACCEPT, "application/json")
                .query(&[("page", page_param.as_str())])
                .send()
                .await
                .map_err(|e| BillingError::Provider(e.to_string()))?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(BillingError::Provider(format!(
                    "xero responded {status}: {body}"
                )));
            }
            let parsed: BankTransactionsResponse = resp
                .json()
                .await
                .map_err(|e| BillingError::Provider(e.to_string()))?;
            if parsed.bank_transactions.is_empty() {
                break;
            }
            for summary in parsed.bank_transactions {
                if let Some(transaction) = summary.into_trust_transaction() {
                    transactions.push(transaction);
                }
            }
            page = page.saturating_add(1);
        }
        Ok(transactions)
    }
}

/// Xero's `GET /BankTransactions` response.
#[derive(Deserialize)]
struct BankTransactionsResponse {
    #[serde(rename = "BankTransactions", default)]
    bank_transactions: Vec<BankTransactionSummary>,
}

#[derive(Deserialize)]
struct BankTransactionSummary {
    #[serde(rename = "BankTransactionID")]
    bank_transaction_id: String,
    /// `RECEIVE`, `SPEND`, and the prepayment/overpayment variants of each.
    #[serde(rename = "Type", default)]
    transaction_type: Option<String>,
    #[serde(rename = "Status", default)]
    status: Option<String>,
    #[serde(rename = "Reference", default)]
    reference: Option<String>,
    #[serde(rename = "Total", default)]
    total: Option<f64>,
    #[serde(rename = "CurrencyCode", default)]
    currency_code: Option<String>,
    #[serde(rename = "Date", default)]
    date: Option<String>,
    #[serde(rename = "BankAccount", default)]
    bank_account: Option<BankAccountRef>,
    #[serde(rename = "LineItems", default)]
    line_items: Vec<BankTransactionLine>,
}

#[derive(Deserialize)]
struct BankAccountRef {
    #[serde(rename = "AccountID", default)]
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct BankTransactionLine {
    #[serde(rename = "Description", default)]
    description: Option<String>,
    #[serde(rename = "LineAmount", default)]
    line_amount: Option<f64>,
    /// The line's own reference — how a pooled withdrawal names which
    /// invoice each part of it settles.
    #[serde(rename = "Reference", default)]
    reference: Option<String>,
}

impl BankTransactionSummary {
    /// `None` for a transaction the mirror has no business posting: one that
    /// is not a plain receive or spend, one Xero has deleted or left as a
    /// draft, or one with no bank account or date to anchor it.
    fn into_trust_transaction(self) -> Option<TrustBankTransaction> {
        let kind = match self
            .transaction_type
            .as_deref()?
            .to_ascii_uppercase()
            .as_str()
        {
            "RECEIVE" => TrustTransactionKind::Receive,
            "SPEND" => TrustTransactionKind::Spend,
            _ => return None,
        };
        if matches!(
            self.status
                .as_deref()
                .map(str::to_ascii_uppercase)
                .as_deref(),
            Some("DELETED" | "VOIDED")
        ) {
            return None;
        }
        let account_id = self.bank_account?.account_id?;
        let occurred_at = parse_xero_datetime(self.date.as_deref()?)?;
        Some(TrustBankTransaction {
            transaction_id: self.bank_transaction_id,
            kind,
            account_id,
            reference: self.reference.unwrap_or_default(),
            amount_cents: self.total.map(dollars_to_cents).unwrap_or_default(),
            currency: self.currency_code.unwrap_or_else(|| "USD".to_string()),
            occurred_at,
            line_items: self
                .line_items
                .into_iter()
                .map(|line| TrustTransactionLine {
                    description: line.description.unwrap_or_default(),
                    amount_cents: line.line_amount.map(dollars_to_cents).unwrap_or_default(),
                    invoice_reference: line.reference,
                })
                .collect(),
        })
    }
}

/// Xero's `GET /Accounts` response.
#[derive(Deserialize)]
struct AccountsResponse {
    #[serde(rename = "Accounts", default)]
    accounts: Vec<AccountSummary>,
}

/// One bank account as Xero reports it. Only the fields the IOLTA mirror
/// reads; Xero returns many more.
#[derive(Deserialize)]
struct AccountSummary {
    #[serde(rename = "AccountID")]
    account_id: String,
    #[serde(rename = "Code", default)]
    code: Option<String>,
    #[serde(rename = "Name", default)]
    name: Option<String>,
    #[serde(rename = "CurrencyCode", default)]
    currency_code: Option<String>,
    /// Xero omits this on accounts it has no balance for; absent reads as
    /// zero rather than failing the whole nightly mirror.
    #[serde(rename = "Balance", default)]
    balance: Option<f64>,
}

impl AccountSummary {
    fn into_bank_account(self) -> TrustBankAccount {
        TrustBankAccount {
            account_id: self.account_id,
            code: self.code,
            name: self.name.unwrap_or_default(),
            currency: self.currency_code.unwrap_or_else(|| "USD".to_string()),
            balance_cents: self.balance.map(dollars_to_cents).unwrap_or_default(),
        }
    }
}

/// Convert a provider decimal amount (e.g. `333.30`) to whole cents. The
/// one float touch is at this wire boundary, rounded immediately to an
/// integer so no float arithmetic enters the money path.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // rounded first; amounts sit far below i64::MAX
fn dollars_to_cents(amount: f64) -> i64 {
    (amount * 100.0).round() as i64
}

/// Parse a Xero date at the JSON wire. Accepts `YYYY-MM-DD`,
/// `YYYY-MM-DDTHH:MM:SS`, RFC 3339, or the Microsoft
/// `\/Date(milliseconds[+offset])\/` form.
fn parse_xero_datetime(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if let Some(inner) = raw
        .strip_prefix("/Date(")
        .and_then(|rest| rest.strip_suffix(")/"))
    {
        let mut end = 0;
        let bytes = inner.as_bytes();
        if bytes.first() == Some(&b'-') {
            end = 1;
        }
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        let millis: i64 = inner.get(..end)?.parse().ok()?;
        return DateTime::from_timestamp_millis(millis);
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        return Some(ndt.and_utc());
    }
    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|ndt| ndt.and_utc())
}

#[cfg(test)]
mod tests {
    use super::{
        dollars_to_cents, parse_xero_datetime, BillingProvider, CachedToken, ContactId,
        ContactRequest, DisabledBillingProvider, StubBillingProvider, XeroBillingProvider,
    };
    use chrono::{TimeZone, Utc};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The billed party for an estate matter — an organisation Xero
    /// dedupes on its unique name.
    fn acme_contact() -> ContactRequest {
        ContactRequest {
            name: "Acme LLC".into(),
            email: "ap@acme.example".into(),
        }
    }

    #[tokio::test]
    async fn disabled_provider_refuses_contacts_and_trust_reads() {
        let provider = DisabledBillingProvider;
        assert!(provider.ensure_contact(&acme_contact()).await.is_err());
        assert!(provider.list_bank_accounts().await.is_err());
        assert!(provider.list_trust_transactions().await.is_err());
    }

    fn xero(base_url: String) -> XeroBillingProvider {
        XeroBillingProvider::new(base_url, "tenant-guid", "TOKEN")
    }

    #[test]
    fn cached_token_is_fresh_strictly_before_refresh_at() {
        let ct = CachedToken {
            token: "tok".into(),
            refresh_at: 100,
        };
        assert_eq!(ct.fresh_at(99), Some("tok"), "fresh before refresh_at");
        assert_eq!(ct.fresh_at(100), None, "must re-mint at the boundary");
        assert_eq!(ct.fresh_at(101), None, "must re-mint past expiry");
    }

    #[tokio::test]
    async fn stub_ensure_contact_records_calls_and_is_deterministic_on_name() {
        let stub = StubBillingProvider::new();
        let a = stub.ensure_contact(&acme_contact()).await.unwrap();
        let b = stub.ensure_contact(&acme_contact()).await.unwrap();
        assert_eq!(a, b, "same email resolves to the same contact id");
        assert_eq!(a, ContactId("stub-contact-ap@acme.example".into()));

        let calls = stub.contact_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], acme_contact());
    }

    #[test]
    fn contact_body_carries_name_and_email() {
        let body =
            xero("https://api.xero.com/api.xro/2.0".into()).build_contact_body(&acme_contact());
        assert_eq!(body["Name"], "Acme LLC");
        assert_eq!(body["EmailAddress"], "ap@acme.example");
    }

    #[tokio::test]
    async fn ensure_contact_returns_existing_id_without_creating() {
        let server = MockServer::start().await;
        // Looked up by the payer's email (the match key).
        Mock::given(method("GET"))
            .and(path("/Contacts"))
            .and(query_param("where", "EmailAddress==\"ap@acme.example\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Contacts": [ { "ContactID": "c-existing", "Name": "Acme LLC" } ],
            })))
            .expect(1)
            .mount(&server)
            .await;
        // No POST mock is mounted: a create attempt would 404 and fail.
        let id = xero(server.uri())
            .ensure_contact(&acme_contact())
            .await
            .expect("resolve succeeds");
        assert_eq!(id, ContactId("c-existing".into()));
    }

    #[tokio::test]
    async fn ensure_contact_creates_when_none_exists() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Contacts"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "Contacts": [] })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Contacts"))
            .and(header("xero-tenant-id", "tenant-guid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Contacts": [ { "ContactID": "c-new" } ],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let id = xero(server.uri())
            .ensure_contact(&acme_contact())
            .await
            .expect("create succeeds");
        assert_eq!(id, ContactId("c-new".into()));
    }

    #[tokio::test]
    async fn ensure_contact_maps_lookup_error_to_provider_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Contacts"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let err = xero(server.uri())
            .ensure_contact(&acme_contact())
            .await
            .expect_err("a 401 is a provider error");
        assert!(err.to_string().contains("401"), "status surfaces: {err}");
    }

    #[test]
    fn dollars_to_cents_rounds_two_dp_at_the_wire_boundary() {
        assert_eq!(dollars_to_cents(333.30), 33_330);
        assert_eq!(dollars_to_cents(0.0), 0);
        assert_eq!(dollars_to_cents(1111.0), 111_100);
    }

    #[test]
    fn parse_xero_datetime_accepts_date_string_and_microsoft_ticks() {
        let from_string = parse_xero_datetime("2026-06-01T00:00:00").unwrap();
        assert_eq!(
            from_string,
            Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap()
        );
        let from_day = parse_xero_datetime("2026-06-01").unwrap();
        assert_eq!(from_day, Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap());
        let from_ticks = parse_xero_datetime("/Date(1717200000000+0000)/").unwrap();
        assert_eq!(from_ticks.timestamp_millis(), 1_717_200_000_000);
    }
}

//! Black-box integration suite for the billing-provider seam.
//!
//! The crate's inline tests cover each method against the Xero wire
//! format in isolation; these tests exercise the public [`BillingProvider`]
//! trait object the way `billing-workflows` consumes it — multi-step flows
//! (auth → contact → invoice → reconcile read) and the money invariants:
//! minor units at the boundary, floats never entering the money path, and
//! read-only IOLTA lanes failing closed.

use billing::xero_auth::XeroClientCredentials;
use billing::{
    BillingError, BillingProvider, ContactRequest, InvoiceLine, InvoiceRequest,
    StubBillingProvider, TrustTransactionKind, XeroBillingProvider,
};
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TENANT: &str = "tenant-guid";
const PAYER_EMAIL: &str = "payer@example.com";
const MATTER: &str = "sample-matter";

/// A client-credentials provider pointed at `server`, minting from the
/// mocked identity endpoint.
fn client_credentials_provider(server: &MockServer) -> XeroBillingProvider {
    let auth = XeroClientCredentials::new(
        "client-id",
        "client-secret",
        format!("{}/connect/token", server.uri()),
        "accounting.contacts accounting.invoices",
    );
    XeroBillingProvider::with_client_credentials(server.uri(), TENANT, auth)
}

/// Mint one bearer for the mocked identity endpoint. Mounted separately
/// in the tests that don't assert on auth itself.
async fn mount_token_endpoint(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/connect/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "minted-tok",
            "token_type": "Bearer",
            "expires_in": 1800,
        })))
        .mount(server)
        .await;
}

/// The contact find-or-create, invoice POST, and reconcile read mocks.
/// The email lookup misses, so the provider creates contact `contact-1`
/// and bills it; the reconcile read reports `PAID` with 1111.00 dollars —
/// exactly `111_100` cents — issued 2026-06-01.
async fn mount_billing_flow_mocks(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/Contacts"))
        .and(query_param(
            "where",
            format!("EmailAddress==\"{PAYER_EMAIL}\""),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "Contacts": [] })),
        )
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/Contacts"))
        .and(body_partial_json(serde_json::json!({
            "Name": "Acme Corp",
            "EmailAddress": PAYER_EMAIL,
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "Contacts": [{ "ContactID": "contact-1" }] })),
        )
        .expect(1)
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/Invoices"))
        .and(header("authorization", "Bearer minted-tok"))
        .and(header("Xero-Tenant-Id", TENANT))
        .and(body_partial_json(serde_json::json!({
            "Type": "ACCREC",
            "Reference": format!("Matter {MATTER}"),
            "Contact": { "ContactID": "contact-1" },
            "LineItems": [{
                "Description": "Flat fee",
                "Quantity": 1,
                "UnitAmount": "1111.00",
                "AccountCode": "200",
            }],
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "Invoices": [{ "InvoiceID": "inv-1" }] })),
        )
        .expect(1)
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/Invoices/inv-1"))
        .and(header("Xero-Tenant-Id", TENANT))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Invoices": [{
                "InvoiceID": "inv-1",
                "Status": "PAID",
                "Total": 1111.0,
                "AmountPaid": 1111.0,
                "CurrencyCode": "USD",
                "DateString": "2026-06-01T00:00:00",
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}

/// The IOLTA ledger: one pooled trust account with a balance and one
/// without, then a page of bank transactions and an empty second page.
async fn mount_iolta_mocks(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/Accounts"))
        .and(query_param("where", r#"Type=="BANK""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Accounts": [
                {
                    "AccountID": "acc-1",
                    "Code": "090",
                    "Name": "IOLTA Pool",
                    "CurrencyCode": "USD",
                    "Balance": 1234.56,
                },
                { "AccountID": "acc-2", "Name": "No Balance Yet" },
            ]
        })))
        .mount(server)
        .await;
    mount_bank_transaction_pages(server).await;
}

/// Page 1 of the trust ledger: a client deposit into trust, the pooled
/// withdrawal that settles an invoice, a VOIDED row (filtered — voided
/// money never moved), and an unknown type (filtered). Page 2 is empty —
/// the read must stop there, not spin.
async fn mount_bank_transaction_pages(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/BankTransactions"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "BankTransactions": [
                {
                    "BankTransactionID": "txn-deposit",
                    "Type": "RECEIVE",
                    "Status": "AUTHORISED",
                    "Reference": "Matter other-matter",
                    "Total": 5000.0,
                    "CurrencyCode": "USD",
                    "Date": "2026-06-01T00:00:00",
                    "BankAccount": { "AccountID": "acc-1" },
                    "LineItems": [{
                        "Description": "Client deposit",
                        "LineAmount": 5000.0,
                        "Reference": "Matter other-matter",
                    }],
                },
                {
                    "BankTransactionID": "txn-withdrawal",
                    "Type": "SPEND",
                    "Reference": "Matter sample-matter",
                    "Total": 1111.0,
                    "CurrencyCode": "USD",
                    "Date": "2026-06-02T00:00:00",
                    "BankAccount": { "AccountID": "acc-1" },
                    "LineItems": [{
                        "Description": "Pooled withdrawal",
                        "LineAmount": 1111.0,
                    }],
                },
                {
                    "BankTransactionID": "txn-voided",
                    "Type": "RECEIVE",
                    "Status": "VOIDED",
                    "Date": "2026-06-03T00:00:00",
                    "BankAccount": { "AccountID": "acc-1" },
                    "LineItems": [],
                },
                {
                    "BankTransactionID": "txn-unknown",
                    "Type": "ARPC",
                    "Date": "2026-06-04T00:00:00",
                    "BankAccount": { "AccountID": "acc-1" },
                    "LineItems": [],
                },
            ]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/BankTransactions"))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "BankTransactions": [] })),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn matter_billing_flow_mints_one_token_and_carries_tenant_auth() {
    let server = MockServer::start().await;

    // The identity endpoint is hit exactly once across the whole flow —
    // every subsequent REST call reuses the cached bearer.
    Mock::given(method("POST"))
        .and(path("/connect/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "minted-tok",
            "token_type": "Bearer",
            "expires_in": 1800,
        })))
        .expect(1)
        .mount(&server)
        .await;
    mount_billing_flow_mocks(&server).await;

    let provider = client_credentials_provider(&server);
    let provider: &dyn BillingProvider = &provider;

    let contact = provider
        .ensure_contact(&ContactRequest {
            name: "Acme Corp".into(),
            email: PAYER_EMAIL.into(),
        })
        .await
        .expect("contact find-or-create");
    assert_eq!(contact.0, "contact-1");

    let invoice = provider
        .create_invoice(
            Uuid::nil(),
            &InvoiceRequest {
                contact_id: Some(contact.clone()),
                contact_name: "Acme Corp".into(),
                contact_email: PAYER_EMAIL.into(),
                reference: format!("Matter {MATTER}"),
                line_items: vec![InvoiceLine {
                    description: "Flat fee".into(),
                    quantity: 1,
                    unit_amount_cents: 111_100,
                    account_code: "200".into(),
                }],
                issued_date: None,
                due_date: None,
            },
        )
        .await
        .expect("invoice creation");
    assert_eq!(invoice.0, "inv-1");

    let status = provider.get_invoice("inv-1").await.expect("reconcile read");
    assert_eq!(status.status, "PAID");
    assert_eq!(status.amount_cents, Some(111_100));
    assert_eq!(status.amount_paid_cents, 111_100);
    assert_eq!(
        status.issued_at.map(|at| at.to_rfc3339()),
        Some("2026-06-01T00:00:00+00:00".into())
    );
}

#[tokio::test]
async fn iolta_mirror_reads_accounts_and_transactions_in_minor_units() {
    let server = MockServer::start().await;
    mount_token_endpoint(&server).await;
    mount_iolta_mocks(&server).await;

    let provider = client_credentials_provider(&server);
    let provider: &dyn BillingProvider = &provider;

    let accounts = provider.list_bank_accounts().await.expect("bank accounts");
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].account_id, "acc-1");
    assert_eq!(accounts[0].code.as_deref(), Some("090"));
    // The float never enters the money path: 1234.56 dollars is exactly
    // 123456 minor units at the wire boundary.
    assert_eq!(accounts[0].balance_cents, 123_456);
    // Xero omits `Balance` on accounts it has no balance for, and absent
    // reads as zero rather than failing the whole nightly mirror.
    assert_eq!(accounts[1].balance_cents, 0, "absent balance reads as zero");
    assert_eq!(accounts[1].currency, "USD", "absent currency defaults");

    let transactions = provider
        .list_trust_transactions()
        .await
        .expect("trust transactions");
    assert_eq!(
        transactions.len(),
        2,
        "voided and unknown-type rows drop out"
    );
    assert_eq!(transactions[0].transaction_id, "txn-deposit");
    assert_eq!(transactions[0].kind, TrustTransactionKind::Receive);
    assert_eq!(transactions[0].amount_cents, 500_000);
    assert_eq!(
        transactions[0]
            .line_items
            .iter()
            .map(|line| line.invoice_reference.as_deref())
            .collect::<Vec<_>>(),
        [Some("Matter other-matter")],
        "a line's own reference names the invoice it settles"
    );
    assert_eq!(transactions[1].transaction_id, "txn-withdrawal");
    assert_eq!(transactions[1].kind, TrustTransactionKind::Spend);
    assert_eq!(transactions[1].amount_cents, 111_100);
    assert_eq!(
        transactions[1].line_items[0].invoice_reference, None,
        "an unlabelled line settles no invoice"
    );
}

#[tokio::test]
async fn read_only_trust_lanes_map_http_errors_to_provider_errors() {
    let server = MockServer::start().await;
    mount_token_endpoint(&server).await;
    Mock::given(method("GET"))
        .and(path("/Accounts"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/BankTransactions"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;

    let provider = client_credentials_provider(&server);
    let provider: &dyn BillingProvider = &provider;

    // The IOLTA mirror is read-only and fails closed: a provider outage
    // is an error to surface, never a silently empty ledger. Provider
    // payloads stay out of assertion messages — the macro reports the
    // compared operands, and rendering provider data into test logs is
    // cleartext logging of sensitive information.
    let accounts = provider.list_bank_accounts().await;
    assert!(matches!(accounts, Err(BillingError::Provider(_))));
    assert!(accounts.err().unwrap().to_string().contains("500"));

    let transactions = provider.list_trust_transactions().await;
    assert!(matches!(transactions, Err(BillingError::Provider(_))));
    assert!(transactions.err().unwrap().to_string().contains("403"));
}

#[tokio::test]
async fn stub_provider_settlement_invariant_through_the_trait() {
    let provider = StubBillingProvider::new();
    let provider: &dyn BillingProvider = &provider;

    // Contact resolution is deterministic on the payer's email — the
    // same address always returns the same contact, never duplicated.
    let request = ContactRequest {
        name: "Acme Corp".into(),
        email: PAYER_EMAIL.into(),
    };
    let first = provider.ensure_contact(&request).await.unwrap();
    let second = provider.ensure_contact(&request).await.unwrap();
    assert_eq!(first, second);

    // The stub derives the invoice's amount in minor units from the
    // line items — quantity × unit_amount_cents, summed — so a settle
    // path reading it back sees whole cents, never a float.
    let invoice = provider
        .create_invoice(
            Uuid::nil(),
            &InvoiceRequest {
                contact_id: Some(first),
                contact_name: "Acme Corp".into(),
                contact_email: PAYER_EMAIL.into(),
                reference: format!("Matter {MATTER}"),
                line_items: vec![
                    InvoiceLine {
                        description: "Flat fee".into(),
                        quantity: 1,
                        unit_amount_cents: 111_100,
                        account_code: "200".into(),
                    },
                    InvoiceLine {
                        description: "Filing fee".into(),
                        quantity: 2,
                        unit_amount_cents: 2_500,
                        account_code: "200".into(),
                    },
                ],
                issued_date: None,
                due_date: None,
            },
        )
        .await
        .expect("invoice creation");

    let status = provider
        .get_invoice(&invoice.0)
        .await
        .expect("reconcile read");
    assert_eq!(status.status, "AUTHORISED", "create-time default status");
    assert_eq!(status.amount_cents, Some(116_100));
    assert_eq!(status.amount_paid_cents, 0);
    assert_eq!(status.currency.as_deref(), Some("USD"));

    // An id the stub was never told about reads as the create-time
    // default — authorised, nothing paid.
    let unknown = provider.get_invoice("stub-never-created").await.unwrap();
    assert_eq!(unknown.status, "AUTHORISED");
    assert_eq!(unknown.amount_paid_cents, 0);
}

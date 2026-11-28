//! `navigator project invoice` end to end: the CLI binary, the portal's real
//! handlers, PostgreSQL, and a stand-in for Stripe (LAW-506, ENG-1342,
//! ENG-1343, LAW-507).
//!
//! Every Stripe write the portal makes is observed: which account it was
//! scoped to, its idempotency key, and its body. The stand-in keeps each
//! draft's items, so the portal's read-back check sees real totals.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::live::{LiveSite, LIVE_KMS};

#[derive(Default)]
struct Ledger {
    customers: usize,
    /// Invoice id by idempotency key, so a replayed create converges.
    invoices: HashMap<String, String>,
    totals: HashMap<String, i64>,
    statuses: HashMap<String, &'static str>,
    fail_next_invoice: bool,
}

#[derive(Clone, Default)]
struct FakeStripe(Arc<Mutex<Ledger>>);

fn field<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

impl FakeStripe {
    fn invoice(&self, id: &str) -> Value {
        let ledger = self.0.lock().unwrap();
        let total = ledger.totals.get(id).copied().unwrap_or_default();
        json!({
            "id": id, "livemode": false, "auto_advance": false, "currency": "usd",
            "status": ledger.statuses.get(id).copied().unwrap_or("draft"),
            "total": total, "subtotal": total, "amount_paid": 0,
        })
    }
}

impl Respond for FakeStripe {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let path = request.url.path().trim_start_matches("/v1/").to_owned();
        let body = String::from_utf8_lossy(&request.body).into_owned();
        let key = request
            .headers
            .get("Idempotency-Key")
            .and_then(|key| key.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let ok = |value: Value| ResponseTemplate::new(200).set_body_json(value);
        let segments: Vec<&str> = path.split('/').collect();
        match (request.method.as_str(), segments.as_slice()) {
            ("GET", ["account"]) => ok(json!({
                "id": if request.headers.contains_key("Stripe-Account") { "acct_Trust" } else { "acct_Platform" }
            })),
            ("POST", ["customers"]) => {
                let mut ledger = self.0.lock().unwrap();
                ledger.customers += 1;
                ok(json!({"id": format!("cus_{}", ledger.customers), "livemode": false}))
            }
            ("GET", ["customers", id]) => ok(json!({"id": id, "livemode": false})),
            ("GET", ["promotion_codes"]) => {
                ok(json!({"data": [{"id": "promo_spring", "livemode": false}]}))
            }
            ("POST", ["invoices"]) => {
                let mut ledger = self.0.lock().unwrap();
                if std::mem::take(&mut ledger.fail_next_invoice) {
                    return ResponseTemplate::new(500);
                }
                let next = format!("in_{}", ledger.invoices.len() + 1);
                let id = ledger.invoices.entry(key).or_insert(next).clone();
                ok(json!({"id": id, "livemode": false}))
            }
            ("POST", ["invoiceitems"]) => {
                let invoice = field(&body, "invoice").unwrap().to_owned();
                let amount: i64 = field(&body, "amount").unwrap().parse().unwrap();
                *self.0.lock().unwrap().totals.entry(invoice).or_default() += amount;
                ok(json!({"id": format!("ii_{}", Uuid::now_v7().simple()), "livemode": false}))
            }
            ("GET", ["invoices", id]) => ok(self.invoice(id)),
            ("POST", ["invoices", id, verb]) => {
                let status = match *verb {
                    "finalize" => {
                        let ledger = self.0.lock().unwrap();
                        if ledger.totals.get(*id).copied().unwrap_or_default() == 0 {
                            "paid"
                        } else {
                            "open"
                        }
                    }
                    "send" => "open",
                    "void" => "void",
                    "mark_uncollectible" => "uncollectible",
                    _ => return ResponseTemplate::new(404),
                };
                self.0
                    .lock()
                    .unwrap()
                    .statuses
                    .insert((*id).to_owned(), status);
                ok(self.invoice(id))
            }
            _ => ResponseTemplate::new(404),
        }
    }
}

/// Stripe writes to `path`, each as (connected account, body).
async fn writes(server: &MockServer, path: &str) -> Vec<(Option<String>, String)> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| {
            request.method.as_str() == "POST" && request.url.path() == format!("/v1/{path}")
        })
        .map(|request| {
            (
                request
                    .headers
                    .get("Stripe-Account")
                    .map(|account| account.to_str().unwrap().to_owned()),
                String::from_utf8_lossy(&request.body).into_owned(),
            )
        })
        .collect()
}

async fn firm_with_stripe(application_store: &store::application_store::ApplicationStore) -> Uuid {
    let (firm, admin) = store::test_support::seed_firm_with_new_brand(
        application_store,
        &format!("synthetic-invoices-{}", Uuid::now_v7().simple()),
    )
    .await;
    store::firm_secrets::put(
        application_store,
        store::firm_secrets::SecretPutRequest {
            actor_role: store::persons::Role::Admin,
            actor_person_id: Some(admin),
            firm_id: firm,
            provider: store::firm_secrets::IntegrationProvider::Stripe,
            kind: store::firm_secrets::IntegrationSecretKind::StripeOperatingAppKey,
            value: "rk_test_synthetic",
        },
        &cloud::FakeKms::new(LIVE_KMS),
    )
    .await
    .unwrap();
    firm
}

async fn with_client(site: &LiveSite) {
    let application_store = store::application_store::ApplicationStore::uninitialized();
    let client = store::persons::create(
        &application_store,
        &store::persons::NewPerson::with_role(
            "Synthetic Client",
            format!("client-{}@example.com", Uuid::now_v7().simple()),
            store::persons::Role::Client,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &application_store,
        &store::participation::AddParticipantCommand {
            project_id: site.project.id,
            person_id: client.id,
            dri: store::participation::DriRequest::Designate(store::projects::DriSide::Client),
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
}

fn refused(site: &LiveSite, args: &[&str], reason: &str) {
    site.navigator(args)
        .assert()
        .failure()
        .stderr(predicates::str::contains(reason));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // one matter's invoices, drafted, sent, paid, and closed in order
async fn invoices_are_drafted_into_trust_sent_on_review_and_closed_with_reasons() {
    store::test_support::with_postgres(async {
        let stripe = FakeStripe::default();
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(stripe.clone())
            .mount(&server)
            .await;
        // A staging deployment: Stripe runs in sandbox mode, so test keys
        // are accepted and the stand-in's loopback base is honored.
        std::env::set_var("NAVIGATOR_STAGING", "true");
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let trust_firm = firm_with_stripe(&application_store).await;
        let bare_firm = firm_with_stripe(&application_store).await;
        std::env::set_var("STRIPE_DEFAULT", "configured");
        std::env::set_var("NAVIGATOR_STRIPE_API_BASE", server.uri());
        std::env::set_var(
            "NAVIGATOR_STRIPE_TRUST_ROUTES",
            json!([{"firm": trust_firm, "state": "NV", "account": "acct_Trust"}]).to_string(),
        );

        let site = LiveSite::start(Some(trust_firm), None, true).await;
        with_client(&site).await;

        // A retainer: unearned, so trust; nothing is sent.
        let retainer = site.json(&[
            "invoice", "create", "--line", "Retainer=1,500", "--advance", "--terms", "net-15",
            "--memo", "Retainer per the engagement letter",
        ]);
        assert_eq!(retainer["status"], "draft", "{retainer:#}");
        assert_eq!(retainer["destination"], "trust");
        assert_eq!(retainer["amount_cents"], 150_000);
        let drafts = writes(&server, "invoices").await;
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].0.as_deref(), Some("acct_Trust"));
        assert!(drafts[0].1.contains("collection_method=send_invoice"), "{}", drafts[0].1);
        let retainer_id = retainer["id"].as_str().unwrap().to_owned();
        let item_accounts = writes(&server, "invoiceitems").await;
        assert!(item_accounts.iter().all(|(account, _)| account.as_deref() == Some("acct_Trust")));

        // A credited invoice: the write-off and discounts show as reasoned
        // lines, and the client's Stripe Customer is reused.
        let credited = site.json(&[
            "invoice", "create", "--line", "Consultation=500", "--line", "Prep=100x2",
            "--write-off", "2:Courtesy", "--discount", "10%", "--discount-reason", "Referral",
            "--credit", "100", "--credit-reason", "Deposit received",
        ]);
        assert_eq!(credited["amount_cents"], 35_000, "{credited:#}");
        let lines: Vec<_> = credited["line_items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line["description"].as_str().unwrap().to_owned())
            .collect();
        assert!(lines.contains(&"Write-off: Prep — Courtesy".to_owned()), "{lines:?}");
        assert!(lines.contains(&"Credit for prior payment — Deposit received".to_owned()));
        assert_eq!(stripe.0.lock().unwrap().customers, 1, "the client's Customer is reused");

        // A zero-balance invoice finalizes as a record and is never sent.
        let zero = site.json(&[
            "invoice", "create", "--line", "Advice=200", "--credit", "200", "--credit-reason",
            "Paid at the consultation",
        ]);
        assert_eq!(zero["amount_cents"], 0);
        let zero_id = zero["id"].as_str().unwrap().to_owned();
        assert_eq!(site.json(&["invoice", "send", &zero_id])["status"], "paid");
        assert!(writes(&server, "invoices/in_3/send").await.is_empty());

        // A coupon is applied by Stripe from its promotion code.
        let request = Uuid::now_v7().to_string();
        let coupon = ["invoice", "create", "--line", "Review=300", "--coupon", "SPRING", "--request-id", &request];
        let couponed = site.json(&coupon);
        let last = writes(&server, "invoices").await.pop().unwrap();
        assert!(last.1.contains("discounts%5B0%5D%5Bpromotion_code%5D=promo_spring"), "{}", last.1);
        // A replayed request returns the first draft and writes nothing new.
        let before = writes(&server, "invoices").await.len();
        assert_eq!(site.json(&coupon)["id"], couponed["id"]);
        assert_eq!(writes(&server, "invoices").await.len(), before);

        // Nothing has been sent; sending is its own step, and once.
        assert!(writes(&server, "invoices/in_1/send").await.is_empty());
        assert_eq!(retainer["client_visible"], false, "a draft is the lawyer's alone");
        let sent = site.json(&["invoice", "send", &retainer_id]);
        assert_eq!((sent["status"].as_str(), sent["client_visible"].as_bool()), (Some("sent"), Some(true)));
        assert_eq!(site.json(&["invoice", "send", &retainer_id])["status"], "sent");
        assert_eq!(writes(&server, "invoices/in_1/send").await.len(), 1);

        // A partial payment by check, then a void with its reason.
        let today = Utc::now().date_naive().to_string();
        let paid = site.json(&[
            "invoice", "payment", &retainer_id, "--amount", "500", "--received-on", &today,
            "--method", "check", "--reference", "1001", "--reason", "Partial payment by check",
        ]);
        assert_eq!(paid["invoice"]["status"], "sent");
        assert_eq!(paid["invoice"]["amount_paid_cents"], 50_000);
        site.navigator(&["invoice", "void", &retainer_id]).assert().failure();
        let void = site.json(&["invoice", "void", &retainer_id, "--reason", "Reissued as two invoices"]);
        assert_eq!(void["status"], "void");
        assert_eq!(void["status_reason"], "Reissued as two invoices");
        assert_eq!(writes(&server, "invoices/in_1/void").await.len(), 1);

        // Money collected outside Stripe sits on the same ledger.
        let check = site.json(&[
            "invoice", "record", "--method", "check", "--external-id", "2002", "--amount", "75",
            "--issued-on", &today, "--paid",
        ]);
        assert_eq!((check["source"].as_str(), check["status"].as_str()), (Some("check"), Some("paid")));
        let listed = site.json(&["invoice", "list"]);
        assert_eq!(listed.as_array().unwrap().len(), 5, "{listed:#}");

        // A matter with no trust route is refused, never sent to Operating.
        let bare = LiveSite::start(Some(bare_firm), None, true).await;
        with_client(&bare).await;
        refused(&bare, &["invoice", "create", "--line", "Work=100"], "no trust account");
        // Operating needs the responsible lawyer, and never takes an advance.
        let colleague = LiveSite::start(Some(bare_firm), None, false).await;
        with_client(&colleague).await;
        refused(
            &colleague,
            &["invoice", "create", "--line", "Work=100", "--account", "operating", "--reason", "Earned"],
            "responsible lawyer",
        );
        refused(
            &bare,
            &["invoice", "create", "--line", "Deposit=100", "--advance", "--account", "operating", "--reason", "Earned"],
            "never Operating",
        );

        // An approved Operating draft is audited, and a retry stays pinned
        // to it: the same destination, and no second approval.
        stripe.0.lock().unwrap().fail_next_invoice = true;
        let request = Uuid::now_v7().to_string();
        let operating = [
            "invoice", "create", "--line", "Completed work=400", "--account", "operating",
            "--reason", "Earned fees for completed work", "--request-id", &request,
        ];
        bare.navigator(&operating).assert().failure();
        let retried = bare.json(&["invoice", "create", "--line", "Completed work=400", "--request-id", &request]);
        assert_eq!(retried["destination"], "operating", "{retried:#}");
        assert_eq!(retried["status"], "draft");
        let approvals: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT approved_by, reason FROM invoice_destination_approval WHERE project_id=$1",
        )
        .bind(bare.project.id)
        .fetch_all(bare.pg.pool())
        .await
        .unwrap();
        assert_eq!(approvals, [(bare.lawyer, "Earned fees for completed work".to_owned())]);
        let last = writes(&server, "invoices").await.pop().unwrap();
        assert_eq!(last.0, None, "Operating drafts are written to the platform account");

        // Another Firm's lawyer cannot see the matter at all: not by code,
        // and the invoice door itself answers 404 by id.
        site.navigator_on(&bare.project.code, &["invoice", "list"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("no visible matter"));
        let other = reqwest::Client::new()
            .post(format!("{}/app/api/projects/{}/invoices/drafts", site.host, bare.project.id))
            .json(&json!({"request_id": Uuid::now_v7(), "lines": [{"description": "x", "unit_amount_cents": 100}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(other.status(), reqwest::StatusCode::NOT_FOUND);
    })
    .await;
}

//! A signed-in Client's consultations and plans.
//!
//! The page lists each Offering in the services catalog's `purchase` block
//! with what this Client may do about it:
//!
//! - **Complimentary.** A consultation is free, and booking it optional, for
//!   a Client whose session came through Google with the address on their
//!   Person — the address they confirmed as a lead. [`complimentary`] is that
//!   rule. A session opened by an email link ([`crate::lead_confirm`]) or any
//!   other door does not earn it.
//! - **Buy.** Every other Offering, and the consultation for everyone else,
//!   is bought: [`buy`] records the Purchase durably, then raises an
//!   `AUTHORISED` Xero invoice keyed on the Purchase id, so a retry never
//!   raises a second one. Xero emails it and collects payment.
//! - **Active.** [`activate_if_paid`] turns the Purchase on once Xero reports
//!   the invoice `PAID`. A subscription's renewals are then handed to a Xero
//!   repeating invoice that Xero raises and emails itself. The DBOS
//!   `PurchaseActivation` schedule runs this every five minutes; the page
//!   also runs it for the viewer's own open invoices, so a Client who has
//!   just paid sees the result on reload.
//!
//! Navigator raises no other invoice: lawyers bill matters in Xero. This is
//! the one self-serve door, and it bills only what the Client chose to buy at
//! the published price.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use chrono::{DateTime, Months, NaiveDate, TimeDelta, Utc};
use dioxus_server::{render_handler, FullstackState, ServeConfig};
use serde::Deserialize;
use store::persons::{Person, Role};
use store::postgres::PostgresDb;
use store::purchases::{self, Cadence, NewPurchase, Purchase, PurchaseState};
use views::locales::services::{OfferingCadence, OfferingCopy, PurchaseCopy};
use webapp::purchases::{OfferingAction, OfferingRow, PurchasePageContent};

use crate::csrf::CsrfMode;
use crate::oauth::ProviderId;
use crate::{gated, AppState, SessionData};

/// The purchase page.
pub const PURCHASES_PATH: &str = "/purchases";
/// The one-Offering buy command.
pub const BUY_PATH: &str = "/purchases/{offering}";
/// Where a complimentary or paid consultation is booked.
pub const BOOKING_PATH: &str = "/consultation";
/// Days after its date that a purchase invoice, and each renewal, falls due.
pub const DUE_DAYS: u32 = 14;
const CURRENCY: &str = "USD";
const MAX_BODY_BYTES: usize = 4 * 1024;
const PAID: &str = "PAID";

/// Whether this session earns the complimentary consultation: it came through
/// the primary provider (Google) with the Person's own address.
#[must_use]
pub fn complimentary(session: &SessionData, person_email: &str) -> bool {
    session.provider.as_deref() == Some(ProviderId::Primary.slug())
        && session
            .email
            .as_deref()
            .is_some_and(|email| email.trim().eq_ignore_ascii_case(person_email.trim()))
}

/// The published price with its cadence: `$100`, `$29/mo`, `$290/year`.
#[must_use]
pub fn price_label(offering: &OfferingCopy) -> String {
    match offering.cadence {
        OfferingCadence::Once => offering.amount.clone(),
        OfferingCadence::Month => format!("{}/mo", offering.amount),
        OfferingCadence::Year => format!("{}/year", offering.amount),
    }
}

fn cadence(offering: &OfferingCopy) -> Cadence {
    match offering.cadence {
        OfferingCadence::Once => Cadence::Once,
        OfferingCadence::Month => Cadence::Month,
        OfferingCadence::Year => Cadence::Year,
    }
}

fn new_purchase(person: &Person, offering: &OfferingCopy) -> NewPurchase {
    NewPurchase {
        person_id: person.id,
        email: person.email.clone(),
        offering: offering.id.clone(),
        offering_name: offering.name.clone(),
        cadence: cadence(offering),
        amount_cents: offering.amount_cents().unwrap_or(0),
        currency: CURRENCY.to_string(),
        account_code: offering.account_code.clone(),
    }
}

fn line(purchase: &Purchase) -> billing::InvoiceLine {
    billing::InvoiceLine {
        description: purchase.offering_name.clone(),
        quantity: 1,
        unit_amount_cents: purchase.amount_cents,
        account_code: purchase.account_code.clone(),
    }
}

fn reference(purchase: &Purchase) -> String {
    format!(
        "{} {}",
        purchase.offering_name,
        &purchase.id.simple().to_string()[..8]
    )
}

/// Why a purchase could not be made.
#[derive(Debug, thiserror::Error)]
pub enum BuyError {
    #[error(transparent)]
    Store(#[from] purchases::PurchaseError),
    #[error(transparent)]
    Billing(#[from] billing::BillingError),
}

/// Buy `offering` for `person`: record the Purchase, then raise its Xero
/// invoice. An open Purchase of the same Offering is returned as it stands,
/// and an `invoicing` one left by an interrupted attempt is invoiced with the
/// same idempotency key.
///
/// # Errors
/// [`BuyError`] when the store or Xero refuses.
pub async fn buy(
    pg: &PostgresDb,
    billing: &dyn billing::BillingProvider,
    person: &Person,
    offering: &OfferingCopy,
    now: DateTime<Utc>,
) -> Result<Purchase, BuyError> {
    let purchase = purchases::open(pg, &new_purchase(person, offering), now).await?;
    if purchase.state == PurchaseState::Invoiced {
        return Ok(purchase);
    }
    let contact = billing
        .ensure_contact(&billing::ContactRequest {
            name: person.name.clone(),
            email: person.email.clone(),
        })
        .await?;
    let invoice = billing
        .create_invoice(
            purchase.id,
            &billing::InvoiceRequest {
                contact_name: person.name.clone(),
                contact_email: person.email.clone(),
                reference: reference(&purchase),
                line_items: vec![line(&purchase)],
                due_date: Some(now.date_naive() + TimeDelta::days(i64::from(DUE_DAYS))),
            },
        )
        .await?;
    // The payment page is a convenience: Xero emails the invoice either way.
    let url = match billing.online_invoice_url(&invoice.0).await {
        Ok(url) => url,
        Err(error) => {
            tracing::warn!(error = %error, purchase_id = %purchase.id, "online invoice url unavailable");
            None
        }
    };
    Ok(
        purchases::record_invoice(pg, purchase.id, &contact.0, &invoice.0, url.as_deref(), now)
            .await?,
    )
}

/// The first renewal date of a subscription activated on `today`.
#[must_use]
pub fn first_renewal(cadence: Cadence, today: NaiveDate) -> Option<NaiveDate> {
    let months = match cadence {
        Cadence::Once => return None,
        Cadence::Month => 1,
        Cadence::Year => 12,
    };
    today.checked_add_months(Months::new(months))
}

/// Turn an invoiced Purchase on when Xero reports its invoice paid, handing a
/// subscription's renewals to a Xero repeating invoice. Returns whether it is
/// now active. Safe to repeat: the repeating invoice is keyed on the Purchase
/// id, and activating an active Purchase changes nothing.
///
/// # Errors
/// A store or Xero failure, as text naming no client content.
pub async fn activate_if_paid(
    pg: &PostgresDb,
    billing: &dyn billing::BillingProvider,
    purchase: &Purchase,
    now: DateTime<Utc>,
) -> Result<bool, String> {
    if purchase.state == PurchaseState::Active {
        return Ok(true);
    }
    let (PurchaseState::Invoiced, Some(invoice_id)) = (purchase.state, &purchase.xero_invoice_id)
    else {
        return Ok(false);
    };
    let status = billing
        .get_invoice(invoice_id)
        .await
        .map_err(|error| error.to_string())?;
    if !status.status.eq_ignore_ascii_case(PAID) {
        purchases::touch(pg, purchase.id, now)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(false);
    }
    let repeating = match (
        first_renewal(purchase.cadence, now.date_naive()),
        &purchase.xero_contact_id,
    ) {
        (Some(start_date), Some(contact_id)) => Some(
            billing
                .create_repeating_invoice(
                    purchase.id,
                    &billing::RepeatingInvoiceRequest {
                        contact_id: contact_id.clone(),
                        reference: purchase.offering_name.clone(),
                        line_items: vec![line(purchase)],
                        every: if purchase.cadence == Cadence::Year {
                            billing::RepeatEvery::Year
                        } else {
                            billing::RepeatEvery::Month
                        },
                        start_date,
                        due_days: DUE_DAYS,
                    },
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        (Some(_), None) => return Err("a subscription Purchase has no Xero contact".into()),
        (None, _) => None,
    };
    purchases::activate(pg, purchase.id, repeating.as_deref(), now)
        .await
        .map_err(|error| error.to_string())?;
    tracing::info!(
        target: "audit",
        purchase_id = %purchase.id,
        offering = %purchase.offering,
        outcome = "activated",
        "purchase"
    );
    Ok(true)
}

/// The DBOS `PurchaseActivation` schedule's view of the store and Xero.
pub struct StoreActivationSource {
    pg: PostgresDb,
    billing: Arc<dyn billing::BillingProvider>,
}

impl StoreActivationSource {
    #[must_use]
    pub fn new(pg: PostgresDb, billing: Arc<dyn billing::BillingProvider>) -> Self {
        Self { pg, billing }
    }
}

#[async_trait::async_trait]
impl workflows::dbos_engine::purchase_activation::ActivationSource for StoreActivationSource {
    async fn invoiced(
        &self,
    ) -> Result<Vec<uuid::Uuid>, workflows::dbos_engine::purchase_activation::ActivationError> {
        purchases::invoiced(&self.pg, workflows::dbos_engine::purchase_activation::BATCH)
            .await
            .map(|rows| rows.into_iter().map(|purchase| purchase.id).collect())
            .map_err(|_| workflows::dbos_engine::purchase_activation::ActivationError::Queue)
    }

    async fn activate(&self, purchase_id: uuid::Uuid) -> Result<bool, String> {
        let purchase = purchases::get(&self.pg, purchase_id)
            .await
            .map_err(|error| error.to_string())?;
        activate_if_paid(&self.pg, self.billing.as_ref(), &purchase, Utc::now()).await
    }
}

#[derive(Clone)]
struct PageState {
    app: AppState,
    copy: Option<PurchaseCopy>,
    /// Whether invoices go to a real Xero organisation. Without one the
    /// billing provider is the in-process stub, which must never stand in
    /// for a real invoice, so the page refuses rather than sell.
    xero: bool,
}

#[derive(Debug, Deserialize, Default)]
struct BuyForm {
    #[serde(default)]
    _csrf: String,
}

/// Build the purchase page and its buy command. `copy` is the mounted
/// brand's catalog `purchase` block; the page refuses when it has none.
pub fn routes(state: &AppState, copy: Option<PurchaseCopy>) -> Router {
    let xero = std::env::var("XERO_TENANT_ID").is_ok_and(|tenant| !tenant.trim().is_empty());
    routes_with(state, copy, xero)
}

fn routes_with(state: &AppState, copy: Option<PurchaseCopy>, xero: bool) -> Router {
    let page_state = PageState {
        app: state.clone(),
        copy: copy.filter(|copy| !copy.offerings.is_empty()),
        xero,
    };
    let render = Router::<FullstackState>::new()
        .route(
            PURCHASES_PATH,
            get(render_handler)
                .layer(from_fn(inject_public_context))
                .layer(from_fn_with_state(page_state.clone(), inject_page_content))
                .layer(from_fn(inject_csrf))
                .layer(from_fn(crate::dioxus_app::dioxus_document_head)),
        )
        .with_state(FullstackState::new(
            ServeConfig::new(),
            webapp::purchases::PurchasePageEntry,
        ));
    let command = Router::new()
        .route(BUY_PATH, post(post_buy))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .route_layer(from_fn_with_state(
            (state.sessions.clone(), CsrfMode::Strict),
            crate::csrf::require_csrf,
        ))
        .route_layer(from_fn_with_state(
            state.rate_limit.clone(),
            crate::rate_limit::enforce,
        ))
        .with_state(page_state);
    gated(state, render).merge(gated(state, command))
}

async fn inject_public_context(mut req: Request, next: Next) -> Response {
    let utility = crate::dioxus_app::public_utility_links(req.extensions().get::<SessionData>());
    req.extensions_mut()
        .insert(webapp::public_chrome::firm_public_chrome(utility));
    next.run(req).await
}

async fn inject_csrf(mut req: Request, next: Next) -> Response {
    let token = req
        .extensions()
        .get::<SessionData>()
        .map_or_else(String::new, |session| session.csrf_token.clone());
    req.extensions_mut()
        .insert(webapp::purchases::PurchasePageCsrf(token));
    next.run(req).await
}

async fn client(state: &PageState, session: &SessionData) -> Option<Person> {
    if session.role != Role::Client {
        return None;
    }
    store::persons::find_by_id(&state.app.surreal, session.person_id?)
        .await
        .ok()
        .flatten()
}

async fn inject_page_content(
    State(state): State<PageState>,
    mut req: Request,
    next: Next,
) -> Response {
    let session = req.extensions().get::<SessionData>().cloned();
    let content = match (&state.copy, session) {
        (Some(copy), Some(session)) => page_content(&state, copy, &session).await,
        (copy, _) => refusal(copy.as_ref()),
    };
    req.extensions_mut()
        .insert(webapp::purchases::InjectedPurchasePage(content));
    next.run(req).await
}

fn shell(copy: &PurchaseCopy) -> PurchasePageContent {
    PurchasePageContent {
        title: copy.title.clone(),
        intro: copy.intro.clone(),
        complimentary: copy.complimentary.clone(),
        book_label: copy.book_label.clone(),
        buy_label: copy.buy_label.clone(),
        pay_label: copy.pay_label.clone(),
        invoiced: copy.invoiced.clone(),
        active: copy.active.clone(),
        refusal: copy.refusal.clone(),
        available: false,
        offerings: Vec::new(),
    }
}

fn refusal(copy: Option<&PurchaseCopy>) -> PurchasePageContent {
    copy.map(shell).unwrap_or_default()
}

async fn page_content(
    state: &PageState,
    copy: &PurchaseCopy,
    session: &SessionData,
) -> PurchasePageContent {
    let (Some(person), true, Ok(pg)) = (
        client(state, session).await,
        state.xero,
        PostgresDb::from_env().await,
    ) else {
        return shell(copy);
    };
    let mut own = purchases::for_person(&pg, person.id)
        .await
        .unwrap_or_default();
    // A Client who just paid sees it on reload, without waiting for the
    // schedule's next slot.
    for purchase in own
        .iter_mut()
        .filter(|p| p.state == PurchaseState::Invoiced)
    {
        let billing = state.app.billing_provider.as_ref();
        if matches!(
            activate_if_paid(&pg, billing, purchase, Utc::now()).await,
            Ok(true)
        ) {
            if let Ok(fresh) = purchases::get(&pg, purchase.id).await {
                *purchase = fresh;
            }
        }
    }
    let waived = complimentary(session, &person.email);
    let rows = copy
        .offerings
        .iter()
        .map(|offering| offering_row(copy, offering, &own, waived))
        .collect();
    PurchasePageContent {
        available: true,
        offerings: rows,
        ..shell(copy)
    }
}

fn offering_row(
    copy: &PurchaseCopy,
    offering: &OfferingCopy,
    own: &[Purchase],
    waived: bool,
) -> OfferingRow {
    let mine = own.iter().filter(|p| p.offering == offering.id);
    let open = mine
        .clone()
        .find(|p| matches!(p.state, PurchaseState::Invoicing | PurchaseState::Invoiced));
    let in_force = mine.clone().any(|p| p.state == PurchaseState::Active);
    let action = if let Some(open) = open {
        OfferingAction::Invoiced {
            pay_url: open.invoice_url.clone(),
        }
    } else if in_force {
        OfferingAction::Active
    } else if offering.complimentary_with_google && waived {
        OfferingAction::Complimentary
    } else {
        OfferingAction::Buy
    };
    OfferingRow {
        id: offering.id.clone(),
        name: offering.name.clone(),
        blurb: offering.blurb.clone(),
        price: price_label(offering),
        google_hint: (offering.complimentary_with_google
            && !waived
            && action == OfferingAction::Buy)
            .then(|| copy.google_hint.clone()),
        action,
    }
}

async fn post_buy(
    State(state): State<PageState>,
    Extension(session): Extension<SessionData>,
    Path(offering_id): Path<String>,
    Form(_form): Form<BuyForm>,
) -> Response {
    let Some(copy) = state.copy.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(offering) = copy.offerings.iter().find(|o| o.id == offering_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(person) = client(&state, &session).await else {
        return (StatusCode::FORBIDDEN, copy.refusal.clone()).into_response();
    };
    let Ok(pg) = PostgresDb::from_env().await else {
        audit(&person, &offering.id, "rejected_unavailable");
        return Redirect::to(PURCHASES_PATH).into_response();
    };
    let now = Utc::now();
    if offering.complimentary_with_google && complimentary(&session, &person.email) {
        return match purchases::record_complimentary(&pg, &new_purchase(&person, offering), now)
            .await
        {
            Ok(_) => {
                audit(&person, &offering.id, "complimentary");
                Redirect::to(BOOKING_PATH).into_response()
            }
            Err(error) => {
                tracing::error!(error = %error, "complimentary purchase failed");
                Redirect::to(PURCHASES_PATH).into_response()
            }
        };
    }
    if !state.xero {
        audit(&person, &offering.id, "rejected_unavailable");
        return Redirect::to(PURCHASES_PATH).into_response();
    }
    match buy(
        &pg,
        state.app.billing_provider.as_ref(),
        &person,
        offering,
        now,
    )
    .await
    {
        Ok(purchase) => {
            audit(&person, &offering.id, "invoiced");
            Redirect::to(purchase.invoice_url.as_deref().unwrap_or(PURCHASES_PATH)).into_response()
        }
        Err(error) => {
            tracing::error!(error = %error, offering = %offering.id, "purchase failed");
            audit(&person, &offering.id, "failed");
            Redirect::to(PURCHASES_PATH).into_response()
        }
    }
}

fn audit(person: &Person, offering: &str, outcome: &str) {
    tracing::info!(
        target: "audit",
        person_id = %person.id,
        offering,
        outcome,
        "purchase"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn session(provider: Option<&str>, email: &str) -> SessionData {
        let mut session = SessionData::fresh("sub", Role::Client);
        session.email = Some(email.to_string());
        session.provider = provider.map(str::to_string);
        session
    }

    #[test]
    fn only_a_google_session_with_the_persons_own_address_is_complimentary() {
        let client = "Visitor@Example.com";
        assert!(complimentary(
            &session(Some("oidc"), "visitor@example.com"),
            client
        ));
        assert!(!complimentary(
            &session(Some("email"), "visitor@example.com"),
            client
        ));
        assert!(!complimentary(
            &session(Some("microsoft"), "visitor@example.com"),
            client
        ));
        assert!(!complimentary(
            &session(None, "visitor@example.com"),
            client
        ));
        assert!(!complimentary(
            &session(Some("oidc"), "someone@example.com"),
            client
        ));
    }

    #[test]
    fn renewals_start_one_period_after_activation() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        assert_eq!(first_renewal(Cadence::Once, today), None);
        assert_eq!(
            first_renewal(Cadence::Month, today),
            NaiveDate::from_ymd_opt(2026, 2, 28)
        );
        assert_eq!(
            first_renewal(Cadence::Year, today).map(|d| d.year()),
            Some(2027)
        );
    }

    async fn database() -> Option<PostgresDb> {
        use sqlx::postgres::{PgConnectOptions, PgSslMode};
        let port = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").ok()?;
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(PgSslMode::Disable);
        let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
        let name = format!("portal_purchase_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
        pg.migrate_embedded(None).await.unwrap();
        Some(pg)
    }

    fn bundled_copy() -> PurchaseCopy {
        views::locales::services::ServicesCatalog::parse(include_str!(
            "../../neon/locales/en/neon/services-catalog.yaml"
        ))
        .unwrap()
        .purchase
    }

    async fn client_person() -> Person {
        let db = store::test_support::mem_surreal().await;
        store::persons::create(
            &db,
            &store::persons::NewPerson::with_role("Pat Client", "pat@example.com", Role::Client),
        )
        .await
        .unwrap()
    }

    #[test]
    fn the_catalog_sells_a_consultation_and_both_delete_your_data_plans() {
        let copy = bundled_copy();
        let rows: Vec<(String, String)> = copy
            .offerings
            .iter()
            .map(|offering| (offering.id.clone(), price_label(offering)))
            .collect();
        assert_eq!(
            rows,
            [
                ("consultation".to_string(), "$100".to_string()),
                ("delete-your-data-monthly".to_string(), "$29/mo".to_string()),
                (
                    "delete-your-data-annual".to_string(),
                    "$290/year".to_string()
                ),
            ]
        );
        assert!(copy.offerings[0].complimentary_with_google);
    }

    #[test]
    fn the_google_hint_shows_only_beside_a_consultation_someone_must_buy() {
        let copy = bundled_copy();
        let consultation = &copy.offerings[0];
        let bought = offering_row(&copy, consultation, &[], false);
        assert!(bought.action == OfferingAction::Buy);
        assert_eq!(
            bought.google_hint.as_deref(),
            Some(copy.google_hint.as_str())
        );
        let free = offering_row(&copy, consultation, &[], true);
        assert!(free.action == OfferingAction::Complimentary);
        assert!(free.google_hint.is_none());
        let plan = offering_row(&copy, &copy.offerings[1], &[], true);
        assert!(
            plan.action == OfferingAction::Buy,
            "only the consultation is ever waived"
        );
        assert!(plan.google_hint.is_none());
    }

    #[tokio::test]
    async fn a_plan_is_invoiced_once_then_active_with_xero_renewals_once_paid() {
        let Some(pg) = database().await else { return };
        let stub = billing::StubBillingProvider::new();
        let person = client_person().await;
        let copy = bundled_copy();
        let annual = copy
            .offerings
            .iter()
            .find(|o| o.id == "delete-your-data-annual")
            .unwrap();
        let now = Utc::now();

        let purchase = buy(&pg, &stub, &person, annual, now).await.unwrap();
        assert_eq!(purchase.state, PurchaseState::Invoiced);
        assert_eq!(purchase.amount_cents, 29_000);
        let again = buy(&pg, &stub, &person, annual, now).await.unwrap();
        assert_eq!(again.id, purchase.id);
        let calls = stub.calls();
        assert_eq!(calls.len(), 1, "a second click raises no second invoice");
        assert_eq!(
            calls[0].matter_id, purchase.id,
            "the Purchase id is the idempotency key"
        );
        assert_eq!(calls[0].request.line_items[0].unit_amount_cents, 29_000);
        assert_eq!(calls[0].request.contact_email, "pat@example.com");
        assert!(calls[0].request.due_date.is_some());
        let invoice_id = purchase.xero_invoice_id.clone().unwrap();
        assert_eq!(
            purchase.invoice_url,
            Some(format!("https://in.xero.test/{invoice_id}"))
        );

        assert!(
            !activate_if_paid(&pg, &stub, &purchase, now).await.unwrap(),
            "unpaid stays off"
        );
        assert!(stub.repeating_calls().is_empty());

        stub.set_invoice_status(
            &invoice_id,
            billing::InvoiceStatus {
                status: "PAID".into(),
                amount_paid_cents: 29_000,
            },
        );
        assert!(activate_if_paid(&pg, &stub, &purchase, now).await.unwrap());
        let active = purchases::get(&pg, purchase.id).await.unwrap();
        assert_eq!(active.state, PurchaseState::Active);
        let repeating = stub.repeating_calls();
        assert_eq!(repeating.len(), 1);
        assert_eq!(repeating[0].key, purchase.id);
        assert_eq!(repeating[0].request.every, billing::RepeatEvery::Year);
        assert_eq!(
            repeating[0].request.start_date,
            first_renewal(Cadence::Year, now.date_naive()).unwrap()
        );
        assert_eq!(
            active.xero_repeating_invoice_id,
            Some(format!("stub-repeating-{}", purchase.id))
        );

        assert!(
            activate_if_paid(&pg, &stub, &active, now).await.unwrap(),
            "a repeat is a no-op"
        );
        assert_eq!(stub.repeating_calls().len(), 1);
    }

    #[tokio::test]
    async fn a_paid_consultation_activates_without_renewals() {
        let Some(pg) = database().await else { return };
        let stub = billing::StubBillingProvider::new();
        let person = client_person().await;
        let copy = bundled_copy();
        let purchase = buy(&pg, &stub, &person, &copy.offerings[0], Utc::now())
            .await
            .unwrap();
        stub.set_invoice_status(
            purchase.xero_invoice_id.as_deref().unwrap(),
            billing::InvoiceStatus {
                status: "PAID".into(),
                amount_paid_cents: 10_000,
            },
        );
        assert!(activate_if_paid(&pg, &stub, &purchase, Utc::now())
            .await
            .unwrap());
        assert!(stub.repeating_calls().is_empty());
        let rows = purchases::for_person(&pg, person.id).await.unwrap();
        let row = offering_row(&copy, &copy.offerings[0], &rows, false);
        assert!(row.action == OfferingAction::Active);
    }
}

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
//! - **Buy.** Every paid Purchase collects into the Firm's Stripe Operating
//!   account. A consultation is a sent Stripe invoice whose hosted page is the
//!   pay link; a recurring plan uses hosted Stripe Checkout with explicit
//!   renewal consent and online cancellation.
//! - **Active.** Provider-verified payment turns the Purchase on. Stripe access
//!   also requires an unexpired paid period. The durable refresh queue and the
//!   purchase page refresh provider status.
//! - **Cancel.** Every authenticated Person can stop their own renewal after
//!   provider confirmation. A role change cannot block cancellation.

use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use billing::stripe::{InvoiceCustomer, OneOffInvoice};
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

/// Buy a one-time `offering` for `person`: reserve the Purchase on the Firm's
/// Stripe Operating account, then send its Stripe invoice. An open Purchase
/// of the same Offering is returned as it stands, and an `invoicing` one left
/// by an interrupted attempt is sent under the same idempotency key.
///
/// # Errors
/// [`BuyError`] when the store or Stripe refuses, or an interrupted attempt
/// has outlived Stripe's idempotency window and needs reconciliation.
pub async fn buy(
    application_store: &store::application_store::ApplicationStore,
    pg: &PostgresDb,
    person: &Person,
    offering: &OfferingCopy,
    now: DateTime<Utc>,
) -> Result<Purchase, BuyError> {
    let firm = crate::subscription_purchases::current_firm(application_store).await?;
    let key = crate::subscription_purchases::credential(application_store, firm).await?;
    let stripe = crate::subscription_purchases::client(&key)?;
    let account = stripe.account_id().await?;
    let purchase =
        purchases::open_stripe_invoice(pg, &new_purchase(person, offering), now, firm, &account)
            .await?;
    if purchase.state == PurchaseState::Invoiced {
        return Ok(purchase);
    }
    // Stripe retains idempotency keys for at least 24 hours. An uncertain
    // first attempt older than that window needs reconciliation, never a
    // second invoice.
    if now >= purchase.inserted_at + TimeDelta::hours(23) {
        return Err(billing::BillingError::Provider(
            "an interrupted purchase invoice requires reconciliation".into(),
        )
        .into());
    }
    let sent = stripe
        .send_one_off_invoice(&OneOffInvoice {
            key: purchase.id,
            customer: InvoiceCustomer::New {
                name: &person.name,
                email: &person.email,
            },
            description: &reference(&purchase),
            amount_cents: purchase.amount_cents,
            days_until_due: DUE_DAYS,
            metadata: ("navigator_purchase_id", purchase.id.to_string()),
        })
        .await?;
    Ok(purchases::record_stripe_invoice(
        pg,
        purchase.id,
        &sent.id,
        sent.hosted_invoice_url.as_deref(),
        now,
    )
    .await?)
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

/// Turn an invoiced one-time Purchase on when Stripe reports its invoice paid
/// in full. Returns whether it is now active. Safe to repeat: activating an
/// active Purchase changes nothing.
///
/// # Errors
/// A store or Stripe failure, as text naming no client content.
pub async fn activate_if_paid(
    application_store: &store::application_store::ApplicationStore,
    pg: &PostgresDb,
    purchase: &Purchase,
    now: DateTime<Utc>,
) -> Result<bool, String> {
    if purchase.state == PurchaseState::Active {
        return Ok(true);
    }
    let (PurchaseState::Invoiced, Some(binding)) = (purchase.state, &purchase.stripe_invoice)
    else {
        return Ok(false);
    };
    let Some(invoice_id) = binding.invoice_id.as_deref() else {
        return Ok(false);
    };
    let refused = |_| "purchase payment was not confirmed".to_owned();
    let key = crate::subscription_purchases::credential(application_store, binding.firm_id)
        .await
        .map_err(refused)?;
    let stripe = crate::subscription_purchases::client(&key).map_err(refused)?;
    if stripe.account_id().await.map_err(refused)? != binding.account_id {
        return Err("purchase invoice account requires reconciliation".into());
    }
    let invoice = stripe
        .invoice_state(invoice_id, purchase.amount_cents)
        .await
        .map_err(refused)?;
    if !invoice.paid(purchase.amount_cents) {
        purchases::touch(pg, purchase.id, now)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(false);
    }
    purchases::activate_stripe_invoice(pg, purchase.id, now)
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

/// The DBOS `PurchaseActivation` schedule's view of provider payments and renewals.
pub struct StoreActivationSource {
    pg: PostgresDb,
    application_store: store::application_store::ApplicationStore,
}

impl StoreActivationSource {
    #[must_use]
    pub fn new(
        pg: PostgresDb,
        application_store: store::application_store::ApplicationStore,
    ) -> Self {
        Self {
            pg,
            application_store,
        }
    }
}

#[async_trait::async_trait]
impl workflows::dbos_engine::purchase_activation::ActivationSource for StoreActivationSource {
    async fn invoiced(
        &self,
    ) -> Result<Vec<uuid::Uuid>, workflows::dbos_engine::purchase_activation::ActivationError> {
        purchases::payment_refresh_queue(
            &self.pg,
            workflows::dbos_engine::purchase_activation::BATCH,
        )
        .await
        .map(|rows| rows.into_iter().map(|purchase| purchase.id).collect())
        .map_err(|_| workflows::dbos_engine::purchase_activation::ActivationError::Queue)
    }

    async fn activate(&self, purchase_id: uuid::Uuid) -> Result<bool, String> {
        let purchase = purchases::get(&self.pg, purchase_id)
            .await
            .map_err(|error| error.to_string())?;
        if purchase.stripe.is_some() {
            let result =
                crate::subscription_purchases::sync(&self.application_store, &self.pg, &purchase)
                    .await;
            purchases::touch(&self.pg, purchase.id, Utc::now())
                .await
                .map_err(|_| "purchase refresh failed".to_owned())?;
            return result
                .map(|fresh| fresh.is_active(Utc::now()))
                .map_err(|_| "subscription payment was not confirmed".to_owned());
        }
        activate_if_paid(&self.application_store, &self.pg, &purchase, Utc::now()).await
    }
}

#[derive(Clone)]
struct PageState {
    app: AppState,
    copy: Option<PurchaseCopy>,
    /// Whether the deployment collects through a Firm's Stripe Operating
    /// account. Without it the page refuses to sell a one-time Offering.
    stripe: bool,
}

#[derive(Debug, Deserialize, Default)]
struct BuyForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    renewal_consent: String,
}

/// Build the purchase page and its buy command. `copy` is the mounted
/// brand's catalog `purchase` block; the page refuses when it has none.
pub fn routes(state: &AppState, copy: Option<PurchaseCopy>) -> Router {
    let stripe = store::firm_secrets::stripe_billing_enabled(
        store::firm_secrets::IntegrationSecretKind::StripeOperatingAppKey,
    );
    routes_with(state, copy, stripe)
}

fn routes_with(state: &AppState, copy: Option<PurchaseCopy>, stripe: bool) -> Router {
    let page_state = PageState {
        app: state.clone(),
        copy: copy.filter(|copy| !copy.offerings.is_empty()),
        stripe,
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
        .route("/purchases/{purchase}/cancel", post(post_cancel))
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

async fn own_person(state: &PageState, session: &SessionData) -> Option<Person> {
    store::persons::find_by_id(&state.app.application_store, session.person_id?)
        .await
        .ok()
        .flatten()
}

async fn client(state: &PageState, session: &SessionData) -> Option<Person> {
    if session.role != Role::Client {
        return None;
    }
    own_person(state, session).await
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
        renewal_consent_label: copy.renewal_consent_label.clone(),
        cancel_label: copy.cancel_label.clone(),
        renewal_stopped: copy.renewal_stopped.clone(),
        paid_through_label: copy.paid_through_label.clone(),
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
    let (Some(person), Ok(pg)) = (
        own_person(state, session).await,
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
        .filter(|p| p.state == PurchaseState::Invoiced && p.stripe_invoice.is_some())
    {
        if matches!(
            activate_if_paid(&state.app.application_store, &pg, purchase, Utc::now()).await,
            Ok(true)
        ) {
            if let Ok(fresh) = purchases::get(&pg, purchase.id).await {
                *purchase = fresh;
            }
        }
    }
    for purchase in own.iter_mut().filter(|p| p.stripe.is_some()) {
        if let Ok(fresh) =
            crate::subscription_purchases::sync(&state.app.application_store, &pg, purchase).await
        {
            *purchase = fresh;
        }
    }
    let waived = complimentary(session, &person.email);
    let rows = copy
        .offerings
        .iter()
        .map(|offering| {
            let mut row = offering_row(copy, offering, &own, waived);
            if offering.cadence != OfferingCadence::Once
                && row.action == OfferingAction::Buy
                && !crate::subscription_purchases::enabled()
            {
                row.action = OfferingAction::Unavailable;
            }
            if session.role != Role::Client
                && matches!(
                    row.action,
                    OfferingAction::Buy | OfferingAction::Complimentary
                )
            {
                row.action = OfferingAction::Unavailable;
            }
            row
        })
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
    let open = mine.clone().find(|p| {
        matches!(p.state, PurchaseState::Invoicing | PurchaseState::Invoiced)
            || (crate::subscription_purchases::unsettled_renewal(p) && !p.is_active(Utc::now()))
    });
    let in_force = mine.clone().any(|p| p.is_active(Utc::now()));
    let action = if let Some(open) = open {
        OfferingAction::Invoiced {
            pay_url: if open.state == PurchaseState::Active {
                None
            } else {
                open.invoice_url.clone()
            },
        }
    } else if in_force {
        OfferingAction::Active
    } else if offering.complimentary_with_google && waived {
        OfferingAction::Complimentary
    } else {
        OfferingAction::Buy
    };
    let current_stripe = mine
        .clone()
        .find(|p| {
            p.is_active(Utc::now())
                || crate::subscription_purchases::unsettled_renewal(p)
                || matches!(p.state, PurchaseState::Invoicing | PurchaseState::Invoiced)
        })
        .and_then(|p| p.stripe.as_ref());
    OfferingRow {
        id: offering.id.clone(),
        name: offering.name.clone(),
        blurb: offering.blurb.clone(),
        price: price_label(offering),
        recurring: offering.cadence != OfferingCadence::Once,
        cancel_purchase_id: mine
            .clone()
            .find(|p| {
                (p.is_active(Utc::now()) || crate::subscription_purchases::unsettled_renewal(p))
                    && p.cadence != Cadence::Once
            })
            .map(|p| p.id.to_string()),
        cancellation_pending: mine.clone().any(|p| {
            p.is_active(Utc::now())
                && (p.renewal_cancelled_at.is_some()
                    || p.stripe.as_ref().is_some_and(|s| s.cancel_at_period_end))
        }),
        paid_through: current_stripe
            .and_then(|stripe| stripe.period_end)
            .or_else(|| {
                mine.clone()
                    .find(|p| p.is_active(Utc::now()))
                    .and_then(|p| p.access_ends_at)
            })
            .map(|end| end.format("%Y-%m-%d %H:%M").to_string()),
        renewal_terms: if action == OfferingAction::Active || current_stripe.is_some() {
            current_stripe.map(|stripe| stripe.renewal_terms.clone())
        } else {
            renewal_terms(offering)
        },
        google_hint: (offering.complimentary_with_google
            && !waived
            && action == OfferingAction::Buy)
            .then(|| copy.google_hint.clone()),
        action,
    }
}

fn renewal_terms(offering: &OfferingCopy) -> Option<String> {
    offering
        .renewal_terms
        .as_deref()
        .filter(|terms| !terms.trim().is_empty())
        .map(|terms| {
            terms.replace("{amount}", &offering.amount).replace(
                "{cancel_url}",
                &format!(
                    "{}/purchases",
                    views::brand::base_url().trim_end_matches('/')
                ),
            )
        })
}

fn renewal_accepted(offering: &OfferingCopy, form: &BuyForm) -> bool {
    offering.cadence == OfferingCadence::Once
        || (offering
            .renewal_terms
            .as_deref()
            .is_some_and(|terms| !terms.trim().is_empty())
            && form.renewal_consent == "accepted")
}

async fn post_buy(
    State(state): State<PageState>,
    Extension(session): Extension<SessionData>,
    Path(offering_id): Path<String>,
    Form(form): Form<BuyForm>,
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
    if !renewal_accepted(offering, &form) {
        return (StatusCode::BAD_REQUEST, copy.refusal.clone()).into_response();
    }
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
    if offering.cadence != OfferingCadence::Once {
        let Some(terms) = renewal_terms(offering) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        return match crate::subscription_purchases::checkout(
            &state.app.application_store,
            &pg,
            &new_purchase(&person, offering),
            &terms,
        )
        .await
        {
            Ok(url) => Redirect::to(&url).into_response(),
            Err(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Subscription checkout is unavailable. Please reload or contact our office.",
            )
                .into_response(),
        };
    }
    if !state.stripe {
        audit(&person, &offering.id, "rejected_unavailable");
        return Redirect::to(PURCHASES_PATH).into_response();
    }
    match buy(&state.app.application_store, &pg, &person, offering, now).await {
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

async fn post_cancel(
    State(state): State<PageState>,
    Extension(session): Extension<SessionData>,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    let Some(person) = own_person(&state, &session).await else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let Ok(pg) = PostgresDb::from_env().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(purchase) = purchases::get(&pg, id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if purchase.person_id != person.id || purchase.cadence == Cadence::Once {
        return StatusCode::NOT_FOUND.into_response();
    }
    if purchase.renewal_cancelled_at.is_some() {
        return Redirect::to(PURCHASES_PATH).into_response();
    }
    // A plan bought before Stripe renews outside Navigator, so its
    // cancellation is the Firm's to confirm by hand.
    let result = if purchase.stripe.is_some() {
        crate::subscription_purchases::cancel(&state.app.application_store, &pg, &purchase).await
    } else {
        Err(billing::BillingError::Provider(
            "a plan bought before Stripe is cancelled by the Firm".into(),
        ))
    };
    match result {
        Ok(()) => Redirect::to(PURCHASES_PATH).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Cancellation was not confirmed. Please try again or contact our office.",
        )
            .into_response(),
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
    fn renewal_disclosure_uses_the_current_catalog_price_and_a_direct_cancellation_url() {
        let copy = bundled_copy();
        let mut offering = copy.offerings[1].clone();
        offering.amount = "$45".into();
        let terms = renewal_terms(&offering).unwrap();
        assert!(terms.contains("$45 USD"));
        assert!(!terms.contains("$29 USD"));
        assert!(terms.contains(&format!(
            "{}/purchases",
            views::brand::base_url().trim_end_matches('/')
        )));
        assert!(!terms.contains("{amount}") && !terms.contains("{cancel_url}"));
    }

    #[test]
    fn recurring_checkout_requires_published_terms_and_explicit_consent() {
        let copy = bundled_copy();
        let mut offering = copy
            .offerings
            .iter()
            .find(|o| o.cadence == OfferingCadence::Month)
            .unwrap()
            .clone();
        let accepted = BuyForm {
            renewal_consent: "accepted".into(),
            ..BuyForm::default()
        };
        offering.renewal_terms = None;
        assert!(!renewal_accepted(&offering, &accepted));
        offering.renewal_terms = Some("   ".into());
        assert!(!renewal_accepted(&offering, &accepted));
        offering.renewal_terms =
            Some("Renews monthly. Cancel online before the next invoice.".into());
        assert!(!renewal_accepted(&offering, &BuyForm::default()));
        assert!(renewal_accepted(&offering, &accepted));
        offering.cadence = OfferingCadence::Year;
        assert!(!renewal_accepted(&offering, &BuyForm::default()));
        offering.cadence = OfferingCadence::Once;
        offering.renewal_terms = None;
        assert!(renewal_accepted(&offering, &BuyForm::default()));
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

    fn bundled_copy() -> PurchaseCopy {
        views::locales::services::ServicesCatalog::parse(include_str!(
            "../../neon/locales/en/neon/services-catalog.yaml"
        ))
        .unwrap()
        .purchase
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

    fn purchase(offering: &OfferingCopy, state: PurchaseState) -> Purchase {
        let now = Utc::now();
        Purchase {
            id: uuid::Uuid::now_v7(),
            person_id: uuid::Uuid::now_v7(),
            email: "pat@example.com".into(),
            offering: offering.id.clone(),
            offering_name: offering.name.clone(),
            cadence: cadence(offering),
            amount_cents: offering.amount_cents().unwrap_or(0),
            currency: CURRENCY.into(),
            account_code: offering.account_code.clone(),
            state,
            invoice_url: Some("https://invoice.stripe.com/i/synthetic".into()),
            inserted_at: now,
            updated_at: now,
            stripe: None,
            stripe_invoice: None,
            renewal_cancelled_at: None,
            access_ends_at: None,
            activated_at: None,
        }
    }

    #[test]
    fn an_open_invoice_outranks_an_active_purchase_and_both_outrank_the_waiver() {
        let copy = bundled_copy();
        let consultation = &copy.offerings[0];
        let invoiced = purchase(consultation, PurchaseState::Invoiced);
        let active = purchase(consultation, PurchaseState::Active);
        let row = offering_row(
            &copy,
            consultation,
            &[active.clone(), invoiced.clone()],
            true,
        );
        assert!(
            row.action
                == OfferingAction::Invoiced {
                    pay_url: invoiced.invoice_url.clone()
                }
        );
        assert!(row.google_hint.is_none());
        let row = offering_row(&copy, consultation, std::slice::from_ref(&active), false);
        assert!(row.action == OfferingAction::Active);
        assert!(
            row.google_hint.is_none(),
            "no hint beside a consultation in force"
        );
        // Another Offering's Purchase never changes this row.
        let plan = purchase(&copy.offerings[1], PurchaseState::Active);
        let row = offering_row(&copy, consultation, &[plan], true);
        assert!(row.action == OfferingAction::Complimentary);
        assert_eq!(row.price, "$100");
        assert_eq!(row.name, consultation.name);
    }

    #[test]
    fn an_overdue_subscription_keeps_its_cancellation_control_and_accepted_terms() {
        let copy = bundled_copy();
        let offering = &copy.offerings[1];
        let mut existing = purchase(offering, PurchaseState::Active);
        let now = Utc::now();
        existing.stripe = Some(purchases::StripePurchase {
            firm_id: uuid::Uuid::new_v4(),
            account_id: "acct_synthetic".into(),
            checkout_session_id: Some("cs_test_synthetic".into()),
            customer_id: Some("cus_synthetic".into()),
            subscription_id: Some("sub_synthetic".into()),
            period_end: Some(now - TimeDelta::days(1)),
            cancel_at_period_end: false,
            renewal_terms: "Accepted renewal terms".into(),
            consented_at: now,
        });
        let row = offering_row(&copy, offering, &[existing.clone()], false);
        assert!(matches!(
            row.action,
            OfferingAction::Invoiced { pay_url: None }
        ));
        assert_eq!(row.cancel_purchase_id, Some(existing.id.to_string()));
        assert_eq!(row.renewal_terms.as_deref(), Some("Accepted renewal terms"));
        assert!(!existing.is_active(now));
    }

    #[test]
    fn the_refusal_shell_carries_the_copy_but_offers_nothing() {
        let copy = bundled_copy();
        let page = refusal(Some(&copy));
        assert!(!page.available);
        assert!(page.offerings.is_empty());
        assert_eq!(page.title, copy.title);
        assert_eq!(page.refusal, copy.refusal);
        assert_eq!(page.buy_label, copy.buy_label);
        let blank = refusal(None);
        assert!(!blank.available);
        assert!(blank.title.is_empty());
    }

    // --- the mounted routes ---
}

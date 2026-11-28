//! The purchase page: a signed-in Client's consultations and plans.
//!
//! The portal resolves, per Offering, what this Client may do — buy it, book
//! a complimentary consultation, pay an open invoice, or nothing because it is
//! already in force — and injects that decision. This crate only renders it
//! with the shared public chrome.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::PublicShell;
use crate::public_chrome::{PublicChrome, PublicFooter, PublicHeader};

/// What this Client may do with one Offering.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub enum OfferingAction {
    /// Buy it: the form sends a Stripe invoice or opens Stripe Checkout.
    #[default]
    Buy,
    /// A consultation waived for a Google sign-in. Booking is optional.
    Complimentary,
    /// An invoice is open; pay it at `pay_url`, the hosted Stripe page.
    Invoiced { pay_url: Option<String> },
    /// Paid and in force.
    Active,
    /// This account may manage its purchases but cannot place a new order.
    Unavailable,
}

/// One Offering as this Client sees it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct OfferingRow {
    pub id: String,
    pub name: String,
    pub blurb: String,
    /// The price as published, with its cadence, e.g. `$29 a month`.
    pub price: String,
    pub recurring: bool,
    pub renewal_terms: Option<String>,
    pub cancel_purchase_id: Option<String>,
    pub cancellation_pending: bool,
    pub paid_through: Option<String>,
    pub action: OfferingAction,
    /// Shown to a Client who could earn a complimentary consultation by
    /// signing in with Google.
    pub google_hint: Option<String>,
}

/// The page copy and every Offering row.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct PurchasePageContent {
    #[serde(default)]
    pub renewal_consent_label: String,
    #[serde(default)]
    pub cancel_label: String,
    #[serde(default)]
    pub renewal_stopped: String,
    #[serde(default)]
    pub paid_through_label: String,
    pub title: String,
    pub intro: String,
    pub complimentary: String,
    pub book_label: String,
    pub buy_label: String,
    pub pay_label: String,
    pub invoiced: String,
    pub active: String,
    /// Shown instead of the rows when purchases are unavailable.
    pub refusal: String,
    pub available: bool,
    pub offerings: Vec<OfferingRow>,
}

/// The portal pre-layer's content injection.
#[derive(Clone, Default)]
pub struct InjectedPurchasePage(pub PurchasePageContent);

/// The CSRF token the portal resolved from the ordinary session cookie.
#[derive(Clone, Default)]
pub struct PurchasePageCsrf(pub String);

/// Everything the page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct PurchasePageView {
    pub chrome: PublicChrome,
    pub content: PurchasePageContent,
    pub csrf_token: String,
}

/// Read the route's decision and the session CSRF token.
#[server]
pub async fn purchase_page_view() -> Result<PurchasePageView, ServerFnError> {
    let content = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<InjectedPurchasePage>,
        _,
    >()
    .await
    .map(|axum::Extension(content)| content.0)
    .unwrap_or_default();
    let csrf_token =
        dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<PurchasePageCsrf>, _>()
            .await
            .map(|axum::Extension(token)| token.0)
            .unwrap_or_default();
    Ok(PurchasePageView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content,
        csrf_token,
    })
}

/// The route entry used by the portal's Dioxus SSR router.
#[component]
pub fn PurchasePageEntry() -> Element {
    let resource = use_server_future(purchase_page_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! {
        PurchasePage {
            chrome: view.chrome,
            content: view.content,
            csrf_token: view.csrf_token,
        }
    }
}

/// The pure purchase page.
#[component]
pub fn PurchasePage(
    chrome: PublicChrome,
    content: PurchasePageContent,
    csrf_token: String,
) -> Element {
    let header = rsx! {
        PublicHeader { chrome: chrome.clone() }
    };
    let footer = rsx! { PublicFooter { chrome: chrome.clone() } };
    rsx! {
        document::Title { "{chrome.brand_name} | {content.title}" }
        PublicShell { header, footer,
            article { class: "start-door purchases",
                h1 { "{content.title}" }
                p { class: "start-door__disclosure", "{content.intro}" }
                if content.available {
                    ul { class: "purchases__offerings",
                        for row in content.offerings.iter() {
                            li { class: "purchases__offering", key: "{row.id}",
                                h2 { "{row.name}" }
                                p { "{row.blurb}" }
                                OfferingDecision { row: row.clone(), content: content.clone(), csrf_token: csrf_token.clone() }
                            }
                        }
                    }
                } else {
                    p { class: "start-door__refusal", role: "alert", "{content.refusal}" }
                }
            }
        }
    }
}

#[component]
fn OfferingDecision(row: OfferingRow, content: PurchasePageContent, csrf_token: String) -> Element {
    match row.action.clone() {
        OfferingAction::Unavailable => rsx! { p { role: "status", "{content.refusal}" } },
        OfferingAction::Buy => rsx! {
            p { class: "purchases__price", "{row.price}" }
            form { method: "post", action: "/purchases/{row.id}",
                input { r#type: "hidden", name: "_csrf", value: "{csrf_token}" }
                if row.recurring {
                    if let Some(terms) = row.renewal_terms.as_ref() {
                        p { id: "renewal-{row.id}", class: "purchases__renewal", "{terms}" }
                        label {
                            input { class: "nav-checkbox", r#type: "checkbox", name: "renewal_consent", value: "accepted", required: true, "aria-describedby": "renewal-{row.id}" }
                            "{content.renewal_consent_label}"
                        }
                    } else {
                        p { role: "status", "{content.refusal}" }
                    }
                }
                button { class: "nav-btn nav-btn--primary", r#type: "submit", disabled: row.recurring && row.renewal_terms.is_none(),
                    "{content.buy_label} · {row.price}"
                }
            }
            if let Some(hint) = row.google_hint.clone() {
                p { class: "purchases__hint", "{hint}" }
            }
        },
        OfferingAction::Complimentary => rsx! {
            p { class: "purchases__complimentary", "{content.complimentary}" }
            form { method: "post", action: "/purchases/{row.id}",
                input { r#type: "hidden", name: "_csrf", value: "{csrf_token}" }
                button { class: "nav-btn nav-btn--primary", r#type: "submit", "{content.book_label}" }
            }
        },
        OfferingAction::Invoiced { pay_url } => rsx! {
            p { class: "purchases__status", role: "status", "{content.invoiced}" }
            {renewal_controls(&row, &content, &csrf_token)}
            if let Some(pay_url) = pay_url {
                a { class: "nav-btn nav-btn--primary", href: "{pay_url}", rel: "noopener", "{content.pay_label}" }
            }
        },
        OfferingAction::Active => rsx! {
            p { class: "purchases__status", role: "status", "{content.active}" }
            {renewal_controls(&row, &content, &csrf_token)}
            if row.id == "consultation" {
                a { class: "nav-btn nav-btn--primary", href: "/consultation", "{content.book_label}" }
            }
        },
    }
}

fn renewal_controls(row: &OfferingRow, content: &PurchasePageContent, csrf_token: &str) -> Element {
    rsx! {
            if let Some(terms) = row.renewal_terms.as_ref() {
                p { class: "purchases__renewal", "{terms}" }
            }
            if let Some(end) = row.paid_through.as_ref() {
                p { "{content.paid_through_label}" " {end} UTC" }
            }
            if row.cancellation_pending {
                p { role: "status", "{content.renewal_stopped}" }
            } else if let Some(id) = row.cancel_purchase_id.as_ref() {
                form { method: "post", action: "/purchases/{id}/cancel",
                    input { r#type: "hidden", name: "_csrf", value: "{csrf_token}" }
                    button { r#type: "submit", class: "nav-btn nav-btn--secondary", "{content.cancel_label}" }
                }
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(action: OfferingAction, google_hint: Option<String>) -> PurchasePageContent {
        PurchasePageContent {
            renewal_consent_label: "I agree to renewal and cancellation terms".into(),
            cancel_label: "Cancel renewal".into(),
            renewal_stopped: "Renewal stopped".into(),
            paid_through_label: "Paid access ends".into(),
            title: "Consultations and plans".into(),
            intro: "Intro copy".into(),
            complimentary: "Complimentary copy".into(),
            book_label: "Book a consultation".into(),
            buy_label: "Buy".into(),
            pay_label: "Pay".into(),
            invoiced: "Invoice sent".into(),
            active: "Active".into(),
            refusal: "Refusal copy".into(),
            available: true,
            offerings: vec![OfferingRow {
                id: "consultation".into(),
                name: "Consultation".into(),
                blurb: "Blurb".into(),
                price: "$100".into(),
                recurring: false,
                renewal_terms: None,
                cancel_purchase_id: None,
                cancellation_pending: false,
                paid_through: None,
                action,
                google_hint,
            }],
        }
    }

    fn render(content: PurchasePageContent) -> String {
        let mut dom = VirtualDom::new_with_props(
            PurchasePage,
            PurchasePageProps {
                chrome: PublicChrome {
                    brand_name: "Neon Law".into(),
                    ..PublicChrome::default()
                },
                content,
                csrf_token: "csrf".into(),
            },
        );
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn an_unpaid_subscription_can_be_cancelled_without_another_checkout() {
        let mut page = content(OfferingAction::Invoiced { pay_url: None }, None);
        page.offerings[0].cancel_purchase_id = Some("synthetic".into());
        page.offerings[0].renewal_terms = Some("Accepted terms".into());
        let html = render(page);
        assert!(html.contains("action=\"/purchases/synthetic/cancel\""));
        assert!(html.contains("name=\"_csrf\""));
        assert!(html.contains("Cancel renewal"));
        assert!(html.contains("Accepted terms"));
        assert!(!html.contains("Buy ·"));
    }

    #[test]
    fn a_buyable_offering_posts_to_its_own_route_with_the_price_and_google_hint() {
        let html = render(content(
            OfferingAction::Buy,
            Some("Sign in with Google".into()),
        ));
        assert!(
            html.contains(r#"action="/purchases/consultation""#),
            "{html}"
        );
        assert!(html.contains(r#"name="_csrf""#), "{html}");
        assert!(html.contains("Buy · $100"), "{html}");
        assert!(html.contains("Sign in with Google"), "{html}");
        assert!(!html.contains("Complimentary copy"), "{html}");
    }

    #[test]
    fn recurring_terms_and_consent_are_inside_the_purchase_form_next_to_the_button() {
        let mut page = content(OfferingAction::Buy, None);
        let row = &mut page.offerings[0];
        row.recurring = true;
        row.renewal_terms = Some("Renews monthly. Cancel online before the next invoice.".into());
        let html = render(page.clone());
        let form = html
            .split("<form")
            .nth(1)
            .unwrap()
            .split("</form>")
            .next()
            .unwrap();
        let terms = form.find("Renews monthly.").unwrap();
        let consent = form.find("name=\"renewal_consent\"").unwrap();
        let button = form.find("<button").unwrap();
        assert!(terms < consent && consent < button);
        assert!(form.contains("required"));
        assert!(!form.contains("disabled"));
        page.offerings[0].renewal_terms = None;
        let html = render(page);
        assert!(html.contains("disabled"));
        assert!(!html.contains("name=\"renewal_consent\""));
    }

    #[test]
    fn a_complimentary_consultation_books_without_a_price() {
        let html = render(content(OfferingAction::Complimentary, None));
        assert!(html.contains("Complimentary copy"), "{html}");
        assert!(html.contains(">Book a consultation<"), "{html}");
        assert!(!html.contains("$100"), "{html}");
    }

    #[test]
    fn an_open_invoice_links_to_its_stripe_page_and_a_paid_consultation_books() {
        let html = render(content(
            OfferingAction::Invoiced {
                pay_url: Some("https://invoice.stripe.com/i/abc".into()),
            },
            None,
        ));
        assert!(html.contains("Invoice sent"), "{html}");
        assert!(
            html.contains(r#"href="https://invoice.stripe.com/i/abc""#),
            "{html}"
        );
        assert!(!html.contains("<form"), "{html}");

        let html = render(content(OfferingAction::Active, None));
        assert!(html.contains(r#"href="/consultation""#), "{html}");
        assert!(!html.contains("<form"), "{html}");
    }

    #[test]
    fn unavailable_purchases_show_the_refusal_alone() {
        let mut page = content(OfferingAction::Buy, None);
        page.available = false;
        let html = render(page);
        assert!(html.contains("Refusal copy"), "{html}");
        assert!(!html.contains("<form"), "{html}");
    }
}

//! The purchase page: a signed-in Client's consultations and plans.
//!
//! The portal resolves, per Offering, what this Client may do — buy it, book
//! a complimentary consultation, pay an open invoice, or nothing because it is
//! already in force — and injects that decision. This crate only renders it
//! with the shared public chrome.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{PublicShell, SiteHeader, SiteNavLink};
use crate::public_chrome::{PublicChrome, PublicFooter};

/// What this Client may do with one Offering.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub enum OfferingAction {
    /// Buy it: the form raises a Xero invoice.
    #[default]
    Buy,
    /// A consultation waived for a Google sign-in. Booking is optional.
    Complimentary,
    /// An invoice is open; pay it at `pay_url` when Xero supplied one.
    Invoiced { pay_url: Option<String> },
    /// Paid and in force.
    Active,
}

/// One Offering as this Client sees it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct OfferingRow {
    pub id: String,
    pub name: String,
    pub blurb: String,
    /// The price as published, with its cadence, e.g. `$29 a month`.
    pub price: String,
    pub action: OfferingAction,
    /// Shown to a Client who could earn a complimentary consultation by
    /// signing in with Google.
    pub google_hint: Option<String>,
}

/// The page copy and every Offering row.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct PurchasePageContent {
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
        SiteHeader {
            brand_name: chrome.brand_name.clone(),
            home_href: chrome.home_href.clone(),
            logo_href: chrome.logo_href.clone(),
            destinations: chrome
                .destinations
                .iter()
                .map(|link| SiteNavLink::new(link.label.clone(), link.href.clone()))
                .collect(),
            utility: chrome
                .utility
                .iter()
                .map(|link| SiteNavLink::new(link.label.clone(), link.href.clone()))
                .collect(),
        }
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
        OfferingAction::Buy => rsx! {
            p { class: "purchases__price", "{row.price}" }
            form { method: "post", action: "/purchases/{row.id}",
                input { r#type: "hidden", name: "_csrf", value: "{csrf_token}" }
                button { class: "nav-btn nav-btn--primary", r#type: "submit",
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
            if let Some(pay_url) = pay_url {
                a { class: "nav-btn nav-btn--primary", href: "{pay_url}", rel: "noopener", "{content.pay_label}" }
            }
        },
        OfferingAction::Active => rsx! {
            p { class: "purchases__status", role: "status", "{content.active}" }
            if row.id == "consultation" {
                a { class: "nav-btn nav-btn--primary", href: "/consultation", "{content.book_label}" }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(action: OfferingAction, google_hint: Option<String>) -> PurchasePageContent {
        PurchasePageContent {
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
    fn a_complimentary_consultation_books_without_a_price() {
        let html = render(content(OfferingAction::Complimentary, None));
        assert!(html.contains("Complimentary copy"), "{html}");
        assert!(html.contains(">Book a consultation<"), "{html}");
        assert!(!html.contains("$100"), "{html}");
    }

    #[test]
    fn an_open_invoice_links_to_xero_and_a_paid_consultation_books() {
        let html = render(content(
            OfferingAction::Invoiced {
                pay_url: Some("https://in.xero.com/abc".into()),
            },
            None,
        ));
        assert!(html.contains("Invoice sent"), "{html}");
        assert!(html.contains(r#"href="https://in.xero.com/abc""#), "{html}");
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

//! The firm `/contact` page, migrated to Dioxus SSR (#641 / #730 PR6 —
//! content-backed, per #811).
//!
//! Content-backed like the service pages: the portal router pre-resolves the
//! page's [`ContactContent`] from the mounted brand's addresses and injects it
//! via `ServeConfig::context_providers`; [`contact_page_view`] reads it back.
//! Firm-owned, and it publishes the firm's channels only.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{PublicShell, SiteHeader, SiteNavLink, SocialMeta};
use crate::lead_capture::{LeadCaptureContext, LeadCaptureCopy, LeadCaptureForm};
use crate::public_chrome::{PublicChrome, PublicFooter};

/// The resolved contact content — the portal router builds it from the brand
/// addresses and injects it; the wasm-safe carrier across the server-function
/// boundary.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ContactContent {
    /// The full document `<title>` ("Neon Law | Contact"), pre-formatted
    /// portal-side to match the head.
    pub head_title: String,
    pub meta_description: String,
    /// The `og:description` override; `None` previews `meta_description`.
    #[serde(default)]
    pub og_description: Option<String>,
    /// The page `<h1>` (`contact.title`).
    pub page_title: String,
    pub email_label: String,
    pub phone_label: String,
    pub firm_email: String,
    /// The next step offered alongside the firm's published contact details.
    pub booking_href: String,
    pub booking_label: String,
    /// The firm's published voice line, rendered as written and dialled from
    /// the `tel:` link beside it.
    pub firm_phone: String,
    #[serde(default)]
    pub lead_capture: LeadCaptureCopy,
}

/// The [`ContactContent`] injected into the render context by the portal router.
#[derive(Clone, Default)]
pub struct InjectedContact(pub ContactContent);

/// Everything the page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ContactPageView {
    pub chrome: PublicChrome,
    pub content: ContactContent,
    pub lead_capture: LeadCaptureContext,
}

/// Resolve the chrome from the process brand and the contact content from the
/// injected [`InjectedContact`] context.
#[server]
pub async fn contact_page_view() -> Result<ContactPageView, ServerFnError> {
    let content =
        crate::public_chrome::copy_from_request_or_context(consume_context::<InjectedContact>)
            .await
            .0;
    Ok(ContactPageView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content,
        lead_capture: crate::public_chrome::copy_from_request_or_context(
            consume_context::<LeadCaptureContext>,
        )
        .await,
    })
}

/// The page's route entry.
#[component]
pub fn ContactPageEntry() -> Element {
    let resource = use_server_future(contact_page_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! {
        ContactPage {
            chrome: view.chrome,
            content: view.content,
            lead_capture: view.lead_capture,
        }
    }
}

/// The pure contact page: the firm's contact section inside the public shell.
/// Prop-driven, so it server-renders and unit-tests without a server future.
#[component]
pub fn ContactPage(
    chrome: PublicChrome,
    content: ContactContent,
    lead_capture: LeadCaptureContext,
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
    let footer = rsx! {
        PublicFooter { chrome: chrome.clone() }
    };
    // `tel:` dials digits, not the human spacing the number is written with.
    let firm_tel = format!(
        "tel:{}",
        content
            .firm_phone
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+')
            .collect::<String>()
    );
    rsx! {
        document::Title { "{content.head_title}" }
        document::Meta { name: "description", content: "{content.meta_description}" }
        // The Open Graph / Twitter share card the contact head emitted.
        SocialMeta {
            title: content.head_title.clone(),
            description: content.meta_description.clone(),
            og_description: content.og_description.clone(),
            site_name: chrome.brand_name.clone(),
            image: chrome.social_image.clone(),
        }
        PublicShell { header, footer,
            article { class: "contact-page",
                h1 { "{content.page_title}" }
                section {
                    dl {
                        dt { "{content.email_label}" }
                        dd { "{content.firm_email}" }
                        dt { "{content.phone_label}" }
                        dd {
                            a { href: "{firm_tel}", "{content.firm_phone}" }
                        }
                    }
                }
                a { class: "nav-btn nav-btn--primary", href: "{content.booking_href}", "{content.booking_label}" }
                LeadCaptureForm {
                    copy: content.lead_capture.clone(),
                    context: lead_capture,
                }
            }
        }
    }
}

/// The neutral destination for both accepted and silently rejected lead
/// submissions. It never tells a stranger whether a row was written.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ContactSentPageView {
    pub chrome: PublicChrome,
    pub firm_phone: String,
}

/// Resolve the public chrome and the firm's published phone for the neutral
/// lead-submission destination.
#[server]
pub async fn contact_sent_page_view() -> Result<ContactSentPageView, ServerFnError> {
    let content =
        crate::public_chrome::copy_from_request_or_context(consume_context::<InjectedContact>)
            .await
            .0;
    Ok(ContactSentPageView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        firm_phone: content.firm_phone,
    })
}

/// The route entry for the neutral lead-submission destination.
#[component]
pub fn ContactSentPageEntry() -> Element {
    let resource = use_server_future(contact_sent_page_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! {
        ContactSentPage {
            chrome: view.chrome,
            firm_phone: view.firm_phone,
        }
    }
}

/// The page shown after a lead form submission, regardless of whether the
/// submission was accepted or silently rejected.
#[component]
pub fn ContactSentPage(chrome: PublicChrome, firm_phone: String) -> Element {
    let firm_tel = format!(
        "tel:{}",
        firm_phone
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+')
            .collect::<String>()
    );
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
        document::Title { "{chrome.brand_name} | Contact sent" }
        PublicShell { header, footer,
            article { class: "contact-sent-page",
                h1 { "Thank you" }
                p {
                    "We received your note. Check your email for a link that confirms your address and signs you in, where you can book a consultation. Someone from the firm will reply by email. This does not make you a client yet, and nothing here is legal advice. If it is urgent, call "
                    a { href: "{firm_tel}", "{firm_phone}" }
                    "."
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn html() -> String {
        fn app() -> Element {
            let chrome = PublicChrome {
                brand_name: "Neon Law".to_string(),
                disclaimer: "Attorney advertisement.".to_string(),
                ..PublicChrome::default()
            };
            let content = ContactContent {
                head_title: "Neon Law | Contact".to_string(),
                meta_description: "Reach the firm.".to_string(),
                og_description: None,
                page_title: "Contact".to_string(),
                email_label: "Email".to_string(),
                phone_label: "Phone".to_string(),
                firm_email: "support@example.com".to_string(),
                booking_href: "https://calendar.example/book".to_string(),
                booking_label: "Book Consultation".to_string(),
                firm_phone: "+1 555 010 0100".to_string(),
                lead_capture: LeadCaptureCopy {
                    consent_sentence: "By sending this, you agree that Neon Law may email you about this inquiry. Sending it does not make you a client, and nothing on this page is legal advice. See our Privacy Policy.".to_string(),
                    phone_helper: "Optional. Message frequency varies. Message and data rates may apply. Reply STOP to opt out or HELP for help. Our Privacy Policy and texting terms explain how we text and what we keep.".to_string(),
                    sms_label: "Yes, Neon Law may send me text messages about this inquiry at this number, including automated texts. Texting is not a condition of hiring the firm.".to_string(),
                },
            };
            rsx! { ContactPage { chrome, content, lead_capture: LeadCaptureContext::default() } }
        }
        ssr(app)
    }

    #[test]
    fn renders_the_firm_contact_section_with_booking() {
        let out = html();
        assert!(out.contains("Contact"), "page title: {out}");
        assert!(
            out.contains(r#"href="https://calendar.example/book""#),
            "booking link"
        );
        assert!(out.contains("support@example.com"), "firm address: {out}");
    }

    #[test]
    fn publishes_the_firm_as_the_only_contact_channel() {
        let out = html();
        // The page is the firm's call to action: one inbox, not a directory of
        // the brands the firm is adjacent to.
        assert!(!out.contains("mailto:"), "no email draft link: {out}");
        assert!(
            !out.contains("the foundation"),
            "no Foundation contact section: {out}"
        );
    }

    #[test]
    fn publishes_the_consultation_booking_link() {
        let out = html();
        assert!(out.contains("Book Consultation"), "booking action: {out}");
        assert!(
            !out.contains("Appointment"),
            "no priced appointment offer: {out}"
        );
    }

    #[test]
    fn renders_the_firm_phone_as_a_dialable_tel_link() {
        let out = html();
        // Written with spaces for a reader; dialled as digits.
        assert!(out.contains("+1 555 010 0100"), "phone as written: {out}");
        assert!(
            out.contains(r#"href="tel:+15550100100""#),
            "tel href: {out}"
        );
        assert!(out.contains("Phone"), "phone label");
    }

    #[test]
    fn keeps_github_out_of_the_footer_and_firm_contact_channels() {
        let out = html();
        let contact = out
            .split_once(r#"class="contact-page""#)
            .expect("contact article")
            .1
            .split_once("</article>")
            .expect("contact article closes")
            .0;
        assert!(
            !contact.contains("github"),
            "firm contact channels: {contact}"
        );
        // The GitHub Stars row was part of the removed footer marketing
        // surface; the simplified footer carries only the legal strip.
        assert!(
            !out.contains(r#"href="https://github.com/neon-law/navigator""#),
            "the simplified footer no longer links the repository: {out}"
        );
    }

    #[test]
    fn wraps_the_page_in_the_public_shell_chrome() {
        let out = html();
        assert!(out.contains("site-header"), "header chrome: {out}");
        assert!(out.contains("site-footer__legal"), "footer chrome");
        assert!(
            out.contains(r#"class="contact-page""#),
            "contact article class"
        );
    }

    #[test]
    fn renders_the_lead_form_beside_the_mailto_channel() {
        let out = html();
        assert!(out.contains(r#"action="/leads""#), "lead form: {out}");
        assert!(out.contains(r#"href="/privacy""#), "privacy link: {out}");
        assert!(
            out.contains(r#"href="/privacy#text-messaging-sms""#),
            "sms policy link: {out}"
        );
        assert!(
            out.contains("Yes, Neon Law may send me text messages"),
            "sms copy: {out}"
        );
        assert!(
            out.contains("Attorney advertisement"),
            "advertising notice: {out}"
        );
    }

    #[test]
    fn renders_the_neutral_contact_sent_message() {
        fn app() -> Element {
            rsx! {
                ContactSentPage {
                    chrome: PublicChrome {
                        brand_name: "Neon Law".to_string(),
                        ..PublicChrome::default()
                    },
                    firm_phone: "+ ()".to_string(),
                }
            }
        }
        let out = ssr(app);
        assert!(out.contains("We received your note."), "thanks: {out}");
        assert!(
            out.contains("Check your email for a link"),
            "confirm: {out}"
        );
        assert!(
            out.contains("does not make you a client yet"),
            "thanks: {out}"
        );
        assert!(out.contains(r#"href="tel:+""#), "phone: {out}");
    }
}
